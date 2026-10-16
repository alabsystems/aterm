<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-grid: spec recovery from its 1805 unit tests

> Status 2026-10-15: this is the crate's migration worklist. Each spec in §1 is proved through
> Trust/Clean about the compiled Rust function, and then the tests it subsumes (§2) are deleted.
> The 244 tests in §4 sample no spec; the 240 marked **delete** can go now.

Source: `crates/aterm-grid` (read only). Six recovery passes read every `#[test]` (the manifest
counted 1806 because `tests/grid_suite.rs` has the string `#[test]` in a doc comment; the real
count is 1805) and the code each one reaches. Nothing was compiled or run. The claims below come
from reading the code. This file merges the six passes: specs that were the same property seen
from different files are one row here, and the notation is unified in §0.

Result: **206 specs**, covering **1561 tests**. **244 tests are not spec samples** (§4).
Every path below is repo-relative.

## 0. Notation used in the statements

Cells and colours
- `bmp(ch)` = `ch as u32` if that is ≤ 0xFFFF, else 0xFFFD.
- A cell triple `(d,c,f)` means `char_data()=d`, `colors()=c`, `flags()=f`. `bit10(f)`: `f ∋ WIDE_CONTINUATION`
  (the same bit as `PROTECTED`); `WIDE(x)`: bit 9 of `x.flags()`.
- For `c: PackedColors`: `FG(c)=(c.0>>24)&0xF`, `BG(c)=(c.0>>28)&0xF`, `H(c)` = bit 16 (HAS_EXTRAS). A side is
  Default (mode 0), Indexed(i) (mode 1; index in bits 0-7 for fg, 8-15 for bg) or Rgb (mode 2).
- `MAP = ExtendedStyle::FLAG_ATTR_MAP` (15 `(CellFlags, StyleAttrs)` pairs); `M` = the union of its CellFlags
  (the style bits; excludes WIDE, WIDE_CONTINUATION, COMPLEX, USES_STYLE_ID); `c2a(f)` = ∪{sa | (cf,sa)∈MAP, f ⊇ cf}
  (the body of the `#[cfg(test)]` `cell_flags_to_style_attrs`, used as a spec-only fn).
- `CacheInv(s)` for `s: CurrentStyle`: `cached_colors = Cell::convert_colors(fg,bg)` ∧
  `cached_has_style_extras = (flags & 0x3800 ≠ 0) ∨ fg.is_rgb() ∨ bg.is_rgb()` ∧
  `cached_is_default = fg.is_default() ∧ bg.is_default() ∧ flags.is_empty() ∧ ¬protected`.
  `FullEq(s,t)`: equality on every field, caches included (a spec-level relation; no `PartialEq` is derived).

Damage and selection
- `M = Full | Partial(R,B)`, `B: [0,R) → Option<(left,right)>`: the abstraction of
  `DamageTracker{row_bits,row_bounds}`, with bit x set ⇔ `row_bounds[x].damaged` ⇔ `B(x) ≠ None`.
- `Cov(d)` for `SelectionDamage`: the set of absolute rows d covers; `MAX = MAX_SELECTION_DAMAGE_BANDS = 8`.

Dense rings and extras
- Ring model: for a `ComplexCharRing`/`RgbColorRing` with `V = visible_rows`, `S = stride`, the content is
  `A: [0,V)×[0,S) → T`; the blank is `'\0'`/0; an absent RGB plane is all blank; external row r maps to
  physical row `ring_row(r)`.
- `Ext(x)`: the external-coordinate partial map `(row,col) → CellExtra` that `CellExtras::get` exposes; the
  internal key is `(row+row_offset, col)` and stale keys below `row_offset` are invisible. `L(x)` = the number
  of entries carrying a hyperlink.
- CellExtra record: `(link: None|(url,id), ul: None|Rgb|Idx, fg, bg, cx, marks ≤ 16, ext 13-bit, images)`.

Rows (`Row`, `PageStore`)
- `r = (cells: [Cell; cols], len: u16, flags: RowFlags)`; `r'` is the post-state; `EMPTY = Cell::EMPTY`.
- `Cells(r)`: the `List Cell` model of the row's `PageSlice`. `Region(r)`: the address range of `r.cells`.
  `Live(P)`: the rows allocated from `PageStore P` and not yet dropped.
- `cont(c) = Row::is_cell_wide_continuation(c)` = `c<cols ∧ bit10(cells[c]) ∧ ¬WIDE(cells[c]) ∧ c>0 ∧ WIDE(cells[c-1])`
  (a genuine spacer). `WideInv(r)`: cell c is a genuine WIDE head iff `c+1<cols` and `cont(c+1)`.
- `Orph(s,e) = Row::orphaned_wide_halves(s,e)`: the head s-1 when `cont(s)`, and the continuation e when
  `e<cols ∧ WIDE(cells[e-1])` (e clipped to cols; empty when s ≥ e).
- `settle(L,e,h) = (h ∧ e+1 = L ? e : max(L,e))`: `Row::settle_len_after_write`, the one len rule every writer keeps.
- `tight over [0,k)` = 1 + index of the last non-empty cell below k (0 if none): `Row::recalculate_len_up_to(k)`.

Grid
- `Reach`: grids reachable from `Grid::new` / `with_scrollback` / `with_tiered_scrollback` by any sequence of
  public mutators (writes, line_feed, scroll_*, IL/DL/SU/SD, resize_* in any `ReflowMode`/`ResizePolicy`, erase_*,
  unscroll, limit and budget setters, the offload calls, scroll_display / scroll_to_*). States made by poking
  fields (`storage.cursor_state.scroll_region`, `any_double_width`, `storage.rows[i]`) are outside Reach; a spec
  that covers them says so.
- `Inv(g)`: the conjunction in `src/grid/invariants.rs::assert_invariants` (CursorInBounds, WideCharConsistent,
  WideCharNotAtEnd, ExtrasClaimed, ScrollRegionValid, DisplayOffsetValid, RingBufferValid, KeysCoverRetained,
  the ring_extras/lazy_buffer running sums), strengthened with RingExtrasAligned:
  `|ring_extras| = ring_buffer_scrollback()`.
- Screen-op shorthand: `R` = visible_rows, `C` = cols, cursor `(r,c)`, `pw` = pending_wrap, `[top,bot]` (or `[t,b]`)
  = scroll region, `[L,Rm]` = horizontal margins (`L=0, Rm=C-1` when unset), `m` = the `left_right_margin_mode`
  argument. `EC(r)` = `effective_cols_for_row(r)`; `MC(r)` = `EC(r)-1`. `Ch(i,j)` = resolved char (`' '` for
  empty), `Cell(i,j)`, `Prot(i,j)` = `Row::is_cell_protected` (neighbour-aware), `Wr(i)` = row i's WRAPPED flag
  (row i continues row i-1), `LS(i)` = line size, `Len(i)` = row len. `B` = `cursor_template` (the BCE blank),
  `B_rgb` = its ring bytes. `-sat`/`+sat` = saturating ops.
- `Scr(g)[i]` = `Rows(g)[i]`: visible row i read at display_offset 0 (cells with extras, wrapped flag).
  `H(g) = h_0..h_{N-1}`: history oldest first as `get_history_line(i)` returns it across the store, lazy buffer
  and ring (`N = scrollback_lines()`). `Transcript(g) = H(g) ++ Rows(g)`. The tiers are invisible to these.
- `Logical(g)`: Transcript grouped into logical lines (a row with `is_wrapped()` continues the previous one; a
  non-final member contributes its full content width; an early-wrap hole contributes nothing; the final member
  contributes `cells[..len]`; DECDWL/DECDHL rows are units of their own).
- `Chunk_c(L)`: greedy rewrap of logical line L at width c: each chunk takes `min(c, rest)` cells, minus one when
  the cut would split a WIDE head from its continuation; at c=1 a WIDE head becomes a default blank and its
  continuation and extras are dropped; chunk 0 keeps the head row's wrapped flag, later chunks are wrapped. The
  code-level definition is `chunk-cells-to-rows`.
- `Snapshot(g) = [(text.trim_end(), wrapped) | e ∈ Transcript(g)]`; `Tr(g)` (zero-share specs) = numbered history
  lines oldest first, then screen rows `0..=cursor_row`, blank rows skipped (`zero_store_share.rs::transcript`).
- `V_r = visible_row_view(r)`; `CellText(view,c)` = base char (or complex string) then its combining marks.
  `Obs(row)` = per column (char, visual flags, wide/continuation, complex string, combining marks, hyperlink,
  fg/bg RGB) plus the wrapped flag. `Picture(g)` = (Screen, cursor, pw, saved cursor, scroll region, margins).
  `RGB(g,r,c) = (fg_rgb_at, bg_rgb_at)` (dense ring or map).
- Absolute keys: `visible_to_absolute(v) = absolute_row_counter - rows + v`; `oldest_absolute_row() =
  counter - rows - scrollback_lines()` (saturating); transcript index i has key `oldest + i`; `base_y = counter - R`.
- Line round trip: `X(R) = extract_row_extras(R, extras, r, styles)`, `L(R) = row_to_line_with_stored_extras(R, X(R))`,
  `Mat(L,c)` = `materialize_from_line(L,c)` cells plus extras; `WF(R)`: R satisfies WideCharConsistent and
  ExtrasClaimed and every wide head carries the WIDE bit its writer set. `Units(text)`: the segmentation by
  repeated `advance_grapheme_unit_wide`.
- Tab state: `S = storage.tab_stops` (may be longer than cols), `suppressed = tab_defaults_suppressed`.
- Offload window: from `resize_offloading_scrollback(..) = Some(job)` to the matching
  `reattach_reflowed_scrollback[_or_redetach]` or `abort_reflow_offload`; `p` = display_offset at detach;
  `W` = lines scrolled off during the window; `CAP = Grid::DETACHED_LAZY_CAP` (50 000).
- Cost (`kind = cost`): the production `count_*` increments in `src/test_counters.rs` (only the `take_*` readers
  are `cfg(test)`; `count_scrollback_reflow_sync_lines` is itself `cfg(any(test, feature="testing"))`), treated as
  ghost state. A cost spec states the counter delta across one call. Once proved, the counters become ghost
  variables and the instrumentation is deleted, as the parser pilot did with `PARSER_LOOP_ITERATIONS`.
- `Refines(G, Model, proj)`: `proj(init) = Model.Init` and every public op maps to a `Model.Next` successor
  (`Buggy=0`); this is what `ty trace validate` checks per transition in the conformance tests, stated over all states.

`#[cfg(test)]`-only APIs (no production caller) seen across the crate include `Grid::{write_char_wrap,
write_ascii_run_styled, write_wide_char_at_cursor_packed, write_emoji_run_autowrap, *_with_style_id, scroll_down,
detach_scrollback, backspace, save_cursor, restore_cursor, checkpoint_lines, tab_n, back_tab_n}`,
`move_cursor_by` (`cfg(any(test, fuzz))`), `GridCursorState::{is_tab_stop, save_cursor}`,
`CellExtras::row_has_hyperlinks`, `StyleTable` interning, `PageStore::reset`, `Offset<T>`, `Pin`/`PinnedRange` and
every `GenerationTracker` reader. A spec is stated over the production function such a wrapper delegates to;
a test whose code has no production twin is in §4.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `primary tests` (count; the names are in §2),
`difficulty (Rust features the proof must handle)`, `blocking gaps` (numbers from
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7;
gap 0, the seal that publishes Clean rows, blocks every spec and is listed on every row).

```tsv
id	kind	functions	statement	primary tests	difficulty	blocking gaps
cell-flags-bit-algebra	equivalence	CellFlags::{empty,contains,intersects,union,difference,insert,remove,is_empty,bits,from_bits,is_complex,uses_style_id,extended_flags,has_extended_flags,core_flags}; impl BitAnd/BitOr/BitAndAssign/BitOrAssign/Not for CellFlags	∀ a,b:u16, f=from_bits(a), g=from_bits(b): bits(f)=a; is_empty(f) ⇔ a=0; is_complex(f) ⇔ a&0x8000≠0; uses_style_id(f) ⇔ a&0x4000≠0; extended_flags(f).bits()=a&0x3800 and has_extended_flags(f) ⇔ a&0x3800≠0; core_flags(f).bits()=a&0x3FFF; contains(f,g) ⇔ a&b=b; intersects(f,g) ⇔ a&b≠0; union(f,g).bits()=(f|g).bits()=a|b; difference(f,g).bits()=a&!b; (f&g).bits()=a&b; (!f).bits()=!a. After insert or |= the receiver's bits are a|b, after remove a&!b, after &= a&b. empty()=default()=from_bits(0).	10	u16 bitwise ops on a #[repr(transparent)] newtype; &self/&mut self receivers; std operator trait impls	0,2,3,5
cell-flags-layout	table-property	CellFlags associated consts; CellFlags::{VISUAL_FLAGS_MASK,EXTENDED_FLAGS_MASK,SPACER_RENDITION_MASK}	The const table is fixed (checkpoints and the renderer read it raw): BOLD..DOUBLE_UNDERLINE are bits 0..8 in declaration order; WIDE=1<<9; WIDE_CONTINUATION=PROTECTED=1<<10; SUPERSCRIPT=1<<11, SUBSCRIPT=1<<12, CURLY_UNDERLINE=1<<13; USES_STYLE_ID=1<<14, COMPLEX=1<<15; OVERLINE=SUPERSCRIPT|SUBSCRIPT; DOTTED_UNDERLINE=UNDERLINE|CURLY_UNDERLINE; DASHED_UNDERLINE=DOUBLE_UNDERLINE|CURLY_UNDERLINE; EXTENDED_FLAGS_MASK=0x3800; VISUAL_FLAGS_MASK=0x3FFF. State as a clean{} table def with one defeq ensures per const.	1	const items built with shifts; no runtime code	0,3
wide-continuation-of	table-property	CellFlags::wide_continuation_of; CellFlags::SPACER_RENDITION_MASK	∀ f: f.wide_continuation_of().bits() = (f.bits() & SPACER_RENDITION_MASK) | WIDE_CONTINUATION.bits(), where SPACER_RENDITION_MASK = VISUAL_FLAGS_MASK & !(WIDE|WIDE_CONTINUATION) (bits 0-8 and 11-13). So the spacer keeps every rendition bit of its lead, carries exactly one bit 10, and is never WIDE, COMPLEX or USES_STYLE_ID; empty and PROTECTED both map to WIDE_CONTINUATION. Decidable over 2^16.	2	const fn on a u16 newtype, & and |	0,1,3
packed-color-encoding	contract	PackedColor::{indexed,rgb,is_default,is_indexed,is_rgb,index,rgb_components,DEFAULT_FG,DEFAULT_BG}	∀ i,r,g,b:u8: indexed(i).0=i; rgb(r,g,b).0 = 0x01<<24|r<<16|g<<8|b. ∀ p: is_default(p) ⇔ p.0>>24=0xFF; is_indexed(p) ⇔ p.0>>24=0x00; is_rgb(p) ⇔ p.0>>24=0x01 (at most one holds); index(p)=p.0&0xFF; rgb_components(p)=((p.0>>16)&0xFF,(p.0>>8)&0xFF,p.0&0xFF). Corollaries: indexed(i) is indexed only with index i; rgb(r,g,b) is rgb only with components (r,g,b); DEFAULT_FG/DEFAULT_BG are default only.	3	u32 shifts, masks and casts; tuple return; &self	0,1,2,3,5
packed-colors-side-algebra	contract	PackedColors::{fg_mode,bg_mode,fg_is_*,bg_is_*,fg_index,bg_index,set_fg_indexed,set_bg_indexed,with_rgb_fg,with_rgb_bg,set_fg_default,set_bg_default,is_default,has_extras,with_extras_flag,without_extras_flag}; Cell::set_has_extras	∀ c, i:u8 (FG, BG, H as in §0): fg_is_default/indexed/rgb(c) ⇔ FG(c)=0/1/2 (pairwise exclusive), likewise bg with BG; fg_index(c)=c.0&0xFF, bg_index(c)=(c.0>>8)&0xFF; is_default(c) ⇔ FG(c)=0 ∧ BG(c)=0. set_fg_indexed(c,i) gives FG=1, fg_index=i, all bits outside 0-7 and 24-27 unchanged; with_rgb_fg(c) gives FG=2 and set_fg_default(c) FG=0, all bits outside 24-27 unchanged; bg setters symmetric (bits 8-15, 28-31). Hence each setter leaves the other side and H unchanged, and the last setter on a side decides its read-back whatever the order. with_extras_flag(c).0=c.0|1<<16; without_extras_flag(c).0=c.0&!(1<<16); has_extras(c) ⇔ H(c)≠0. Cell::set_has_extras(b) changes only bit 16 of colors.	2	u32 bitwise ops on a newtype; const fns; Cell is #[repr(C, packed)]	0,2,3,5
convert-colors	contract	Cell::convert_colors; Cell::convert_legacy_colors; Cell::with_style (colors part)	∀ fg,bg:PackedColor, k=convert_colors(fg,bg): the fg side of k is Indexed(fg.index()) if fg.is_indexed(), Rgb if fg.is_rgb(), else Default (any type byte other than 0x00/0x01, 0xFF included); the bg side likewise; has_extras(k)=false and bits 16-23 of k.0 are 0. with_style(ch,fg,bg,f).colors() = convert_colors(fg,bg).	5	if-chains over const fn calls on newtypes	0,1,3,4,5
cell-constructors	contract	Cell::{EMPTY,default,from_ascii_fast,from_ascii_styled,new,with_style,from_raw_parts,char_data,colors,flags}	With triples (d,c,f) as in §0: EMPTY=default()=from_ascii_fast(b' ')=(0x20,DEFAULT,empty); from_ascii_fast(b)=(b,DEFAULT,empty) for every b; from_ascii_styled(b,c,f)=(b,c,f); new(ch)=(bmp(ch),DEFAULT,empty), never COMPLEX; with_style(ch,fg,bg,f)=(bmp(ch),convert_colors(fg,bg),f); from_raw_parts(d,c,f)=(d,c,f), so every cell round-trips through (char_data,colors,flags). Corollary: from_ascii_fast(b)=new(b as char) for b<0x80.	10	#[repr(C, packed)] struct literals, char→u32 casts, const fn	0,1,3,5
cell-read-accessors	contract	Cell::{char,char_data,is_complex,is_wide,is_wide_continuation,is_empty,has_extras,codepoint}	∀ x=(d,c,f): is_complex(x) ⇔ COMPLEX∈f; char(x)=U+FFFD if complex, else char::from_u32(d) or U+FFFD for a surrogate; codepoint(x)=0xFFFD if complex else d (test-only); is_wide ⇔ WIDE∈f; is_wide_continuation ⇔ bit10(f) (raw, not context-aware); has_extras ⇔ bit 16 of c.0; is_empty(x) ⇔ d=0x20 ∧ c.is_default() (mode nibbles only) ∧ f.0=0. So EMPTY, default() and new(' ') are empty, new(ch≠' ') is not, and a complex cell keeps its overflow index in char_data while reading U+FFFD.	4	&self on a packed struct (fields copied out), char::from_u32, Option	0,1,2,3
cell-mutators	contract	Cell::{set_char,set_overflow_index,set_flags,set_has_extras}; Cell::set_fg (testing-only)	set_char(ch) requires ch as u32 ≤ 0xFFFF (debug_assert; release stores 0xFFFD): char_data'=ch, flags'=flags∖{COMPLEX,WIDE,WIDE_CONTINUATION}, colors unchanged; so set_char(ch) on EMPTY = new(ch), and on a complex cell it clears COMPLEX. set_overflow_index(i): char_data'=i, flags'=flags∪COMPLEX, colors unchanged. set_flags(f): flags'=f only. set_has_extras(b): only bit 16 of colors changes, to b. set_fg(p) (testing-only): the fg side follows p's mode; bg side and bit 16 unchanged.	6	&mut self on #[repr(C, packed)] (copy-modify-write); a debug_assert precondition	0,1,2,3,5
cell-color-accessors	contract	Cell::{fg_color,bg_color,fg_needs_overflow,bg_needs_overflow}	∀ x: uses_style_id(x) ⇒ fg_color(x)=bg_color(x)=None. Otherwise fg_color(x)=Some(DEFAULT_FG) for default mode, Some(indexed(fg_index)) for indexed, None otherwise (RGB or unknown); bg_color symmetric with DEFAULT_BG. fg_needs_overflow(x) ⇔ colors.fg_is_rgb(); bg_needs_overflow ⇔ colors.bg_is_rgb(). With convert-colors: with_style(_,fg,bg,_) reads default/indexed colours back, and reads None with needs_overflow=true for RGB.	3	Option<PackedColor> return, calls to const fns, &self	0,1,2,3,4
color-new-to-rgb	contract	style::style_color::Color::{new,to_rgb,DEFAULT_FG,DEFAULT_BG}	∀ r,g,b:u8: Color::new(r,g,b)=Color{r,g,b,a:255} and to_rgb(new(r,g,b))=(r,g,b); DEFAULT_FG=new(255,255,255), DEFAULT_BG=new(0,0,0). (with_alpha and from_rgb are #[cfg(test)] and not part of the spec.)	2	const fn returning a 4-field struct; tuple result	0,1
attrs-to-cell-flags-table	table-property	style::ExtendedStyle::attrs_to_cell_flags; ExtendedStyle::FLAG_ATTR_MAP	∀ a: attrs_to_cell_flags(a) = ∪{cf | (cf,sa)∈MAP, a ⊇ sa}. Corollaries: (1) attrs_to_cell_flags(a) ⊆ M, so it never sets WIDE, WIDE_CONTINUATION, COMPLEX or USES_STYLE_ID (control_query.rs relies on it); (2) ∀ f: attrs_to_cell_flags(c2a(f)) = f ∩ M despite the compound encodings; (3) the reverse c2a(attrs_to_cell_flags(a)) is not the identity (StyleAttrs DOTTED alone returns UL|CURLY|DOTTED), so only the CellFlags-side round trip may be claimed.	3	for-loop over a const array of 15 tuples; bitflags u16; a quantified clause over u16	0,1,3,5,7
style-table-new-holds-only-default	contract	StyleTable::{new,get,extended}; Grid::resolve_style_to_colors	For t=StyleTable::new(): get(t,id) = (id=DEFAULT ? Some(&Style::DEFAULT) : None); extended(t,DEFAULT)=Some(ExtendedStyle::DEFAULT) (fg_type=bg_type=Default); extended(t,id≠DEFAULT)=None. No production path adds an entry (intern, intern_extended, insert_new_style are #[cfg(test)]), so on every production grid ∀ id,x: resolve_style_to_colors(id,x) = (DEFAULT_FG, DEFAULT_BG, x) and the USES_STYLE_ID branch is provably dead (control_query.rs only debug_asserts it).	4	Vec with_capacity/push/get, FxHashMap insert, Option<&T>; the corollary needs a Grid-wide invariant that the table length stays 1	0,1,2,4
current-style-cache-invariant	invariant	CurrentStyle::{default,new,update_cached_colors,update_flags_cache,reset,reset_sgr,has_style_extras,is_default,cached_colors}	CacheInv(default()); ∀ fg,bg,f,p: CacheInv(new(fg,bg,f,p)). After any assignment to the pub fields fg/bg/flags/protected, CacheInv(s.update_cached_colors()). If s.cached_colors = convert_colors(s.fg,s.bg) (only flags/protected changed), CacheInv(s.update_flags_cache()). CacheInv is preserved by reset and reset_sgr. Hence under CacheInv: has_style_extras(s) ⇔ flags hit bits 11-13 or fg or bg is RGB; is_default(s) ⇔ both colours default ∧ flags empty ∧ ¬protected. Plain SGR bits and indexed colours never need extras.	11	struct with private cache fields; &mut self two-state; callee ensures of PackedColor and convert_colors; the invariant concerns writes to pub fields from outside the type	0,1,2,3,4
current-style-new-fields	contract	CurrentStyle::{new,fg_packed,bg_packed,flags_bits,bce_bg_rgb,default}	∀ fg,bg,f,p, s=new(fg,bg,f,p): s.fg=fg, s.bg=bg, s.flags=f, s.protected=p, fg_packed(s)=fg.0, bg_packed(s)=bg.0, flags_bits(s)=f.bits(); bce_bg_rgb(s)=Some([r,g,b]) when bg.is_rgb() with (r,g,b)=bg.rgb_components(), else None. FullEq(new(DEFAULT_FG,DEFAULT_BG,empty,false), default()).	4	struct construction, &self accessors on newtypes, Option<[u8;3]>	0,1,2
current-style-reset	contract	CurrentStyle::{reset,reset_sgr}	∀ s: FullEq(reset(s), default()). reset_sgr(s) is FullEq to default() with protected:=s.protected and cached_is_default:=¬s.protected, so SGR 0 clears colours and flags but keeps DECSCA, and is_default(reset_sgr(s)) ⇔ ¬s.protected. CacheInv holds after both.	3	&mut self whole-struct assignment; two-state clause over the old protected field	0,1,2
display-offset-damage	contract	damage::display_offset::compute_display_offset_damage; Damage::apply_display_offset_damage	∀ old,new:usize, rows:u16, δ=|old-new|: result=None ⇔ old=new; Full ⇔ old≠new ∧ δ≥rows; otherwise new>old gives TopRows(δ) with 0<δ<rows and new<old gives BottomRows{start:rows-δ,end:rows}. The damaged row count is always min(δ,rows). Both u16::try_from(..).expect() calls are unreachable (no panic). apply: None leaves damage unchanged, Full marks full, TopRows(n)=mark_rows(0,n), BottomRows{s,e}=mark_rows(s,e).	6	usize/u16 compare and subtract; enum result; try_from().expect() proved unreachable; &mut self	0,1,3,4
damage-row-iterators	contract	BitsetRowIterator::{new,next,size_hint}; DamagedRowIterator::{next,size_hint}	∀ bits:&[u64], max:u16 with |bits|·64 ≤ 65536: BitsetRowIterator::new(bits,max) yields exactly the r=64w+k with bit k of bits[w] set and r<max, strictly increasing, then None forever (fused). size_hint()=(0,Some(u)) with u = popcount of the unconsumed bits (≥ items remaining), never panics, u=0 once consumed. DamagedRowIterator::Full{current,max} (requires current ≤ max) yields current..max with exact size_hint (max-current); Partial(it) behaves as it.	19	trailing_zeros loop, slice indexing and .get(), Iterator impls, enum dispatch, &mut self	0,1,2,3,7,11
damage-tracker-model	refinement	Damage::{new,mark_full,mark_row,mark_rows,mark_cell,mark_wide_cell,is_full,is_row_damaged,row_damage_bounds,has_damage,reset}; DamageTracker::{new,mark_row,mark_rows,mark_cell,mark_wide_cell,is_row_damaged,row_damage_bounds,clear,damaged_row_count}	Damage/DamageTracker refine M = Full | Partial(R,B) (§0) with R=|row_bounds| and |row_bits|=ceil(R/64). new(R)=Partial(R, all None); mark_full gives Full; on Full every mark is a no-op; on Partial a mark of row x≥R is a no-op. mark_row(x): B(x)=(0,u16::MAX); mark_cell(x,c): B(x)=hull(B(x),[c,c+1 sat)); mark_wide_cell: [c,c+2 sat); mark_rows(s,e)=fold mark_row over s..e (empty if s≥e); reset(R) and clear(R) give Partial(R,all None) from any state; each mark on Partial increments mark_seq (wrapping). Observations: is_row_damaged(x) = Full ∨ (x<R ∧ B(x)≠None); has_damage = Full ∨ ∃x B(x)≠None; damaged_row_count = |{x : B(x)≠None}|; row_damage_bounds(x,cols) = Some((0,cols)) on Full, else B(x)=(l,r) clamped to (min l cols, min r cols), returned only if non-empty.	36	Vec<u64> bitset plus Vec<RowDamageBounds>; enum with &mut self; loop; saturating_add, min, max	0,1,2,3,7
damage-bounds-iteration	contract	Damage::{damaged_rows,iter_bounds}; DamageBoundsIterator::next	∀ d reachable from Damage::new(R) or Full, rows ≤ R, cols: damaged_rows(d,rows) yields exactly [x∈0..rows | is_row_damaged(d,x)] increasing (all of 0..rows on Full); iter_bounds(d,rows,cols) yields LineDamageBounds{line:x,left:l,right:r} in increasing x for exactly those damaged x with row_damage_bounds(d,x,cols)=Some((l,r)). A row whose clamped bounds are empty is skipped and never ends the iteration early. Corollary: a single mark_cell(x,c) yields [(x,c,c+1)] if c<cols, else [].	12	Iterator over a borrowed enum, loop, Option chaining	0,1,2,4,7
selection-damage-union-lattice	contract	SelectionDamage::{union,clears_selection,spill,from_merged}; presentation::merge_bands; BandSet::as_slice	Cov(None)=∅, Cov(All)=all rows, Cov(Band{lo,hi})=[lo,hi], Cov(Bands(s))=∪s. Canonical(d) ⇔ None, All, Band with lo≤hi, or Bands of 2..=MAX bands ascending, disjoint, non-adjacent. ∀ canonical a,b: (1) a.union(b) is canonical; (2) None is the unit and All absorbs, both sides; (3) Cov(a)∪Cov(b) ⊆ Cov(a.union(b)); (4) if the maximal intervals of Cov(a)∪Cov(b) number k ≤ MAX, a.union(b) is exactly those intervals (Band when k=1), so adjacent/overlapping bands coalesce, union commutes, and any fold whose intermediates all fit the bound is order-independent. clears_selection(p) = false for None, true for All, p(lo,hi) for Band, ∃band∈s. p(band) for Bands (a gap is never damage).	5	Copy enum with array payload, [(u64,u64);16], insertion-sort loop, saturating_add, FnMut closure parameter	0,1,3,5,7,13
selection-damage-overflow-absorb	contract	SelectionDamage::from_merged	If the merged interval list B (ascending, disjoint, non-adjacent) has |B| > MAX, from_merged repeatedly merges the adjacent pair with the smallest gap buf[i+1].lo-buf[i].hi (first i on ties) until MAX remain, and returns Bands of them. So the result has exactly MAX bands (never the hull, since MAX ≥ 2), covers every row of B, and each merge adds only the rows of one smallest gap: a row inside a gap strictly larger than every merged gap stays uncovered.	2	while loop with nested argmin loop, copy_within on a fixed array, u8 length conversion	0,1,3,5,7
ring-model	refinement	ComplexCharRing::{new,get,set,clear_rect,shift_region_up,shift_region_down,shift_cols_right,shift_cols_left}; RgbColorRing::{new,fg,bg,fill_runs,clear_rows,clear_rect,shift_*,heap_bytes}; RingSpans::{ring_row,scroll_up,shift_rect_up,shift_rect_down,clear,clear_row,clear_range,copy_row_span}; copy_span, shift_right_in, shift_left_in	The ring refines A:[0,V)×[0,S)→T (§0) under ring_offset<V (or V=0), planes of length V·S; every op requires r<V. get/fg/bg(r,c)=None if c≥S, else A[r][c] when non-blank (RGB unpacked from 0x01RRGGBB). set/set_fg/set_bg are no-ops for c≥S and touch no other row. fill_runs(r,a,b,fg,bg) sets A[r][c]=pack(rgb) for c∈[a,min(b,S)) per present plane; a≥min(b,S) is a no-op and allocates no plane. clear_row(r) blanks row r; clear_range(r,a,b) blanks [a,min(b,S)). scroll_up(n≥V) blanks all and sets ring_offset=0; scroll_up(n<V): A'[r]=A[r+n] for r<V-n, blank above. shift_rect_up(t,b,l,r,n) (t≤b<V, l≤r, n>0; n'=min(n,b-t+1), hi=min(r,S-1)): A'[x][c]=A[x+n'][c] for x∈[t,b-n'], c∈[l,hi]; blank for x∈(b-n',b], c∈[l,min(r+1,S)); unchanged elsewhere; shift_rect_down mirrors; shift_region_* = shift_rect_* with l=0, r=S-1. shift_cols_right(r,s,k,m) (hi=min(m,S), j=min(k,hi-s); k=0 or s≥hi no-op): A'[c]=blank if c<s+j else A[c-j] on [s,hi); shift_cols_left: A'[c]=A[c+j] if c<hi-j else blank. An absent plane stays absent. So the bulk fill/clear paths equal the per-cell loops, heap_bytes included.	15	Vec<char> and Option<Box<[u32]>> planes; slice fill and copy_within; generic helpers over T: Copy; trait default methods; loops	0,1,2,3,7,11
cell-extra-fields	contract	CellExtra::{default,has_data,hyperlink,set_hyperlink,hyperlink_id,set_hyperlink_id,underline_color,set_underline_color,set_underline_color_u32,underline_color_u32,underline_color_index,fg_rgb,set_fg_rgb,bg_rgb,set_bg_rgb,complex_char,set_complex_char,combining,add_combining,set_extended_flags,extended_flags,image,set_image}	CellExtra refines the record of §0; default() is all-empty; has_data(e) ⇔ some field non-empty (ext≠0 counts). Each setter changes only its own field: set_hyperlink(Some u) gives link=(u,None) (old id dropped), None clears; set_hyperlink_id(i) sets the id only when link≠None; set_underline_color(Some rgb) gives ul=Rgb(rgb), None clears; set_underline_color_u32(Some v) gives Idx(v&0xFF) when (v>>24)&0xFF=0x02 and Rgb((v>>16)&0xFF,(v>>8)&0xFF,v&0xFF) otherwise, None clears; underline_color_u32 returns 0x02000000|i for Idx(i), 0x01RRGGBB for Rgb, None otherwise; underline_color() returns only the Rgb form; set_fg_rgb/set_bg_rgb/set_complex_char set or clear their own field; add_combining(c) appends iff |marks|<16; set_extended_flags(x) requires x<1<<13 and gives ext=x; set_image(Some i) gives images=[i], None gives []. A shared link Arc is copy-on-write: mutating one CellExtra never changes another.	12	Option<Arc<..>>, Option<Box<Arc<str>>>, SmallVec<char,2>, [u8;9]; Arc::make_mut; &mut self	0,1,2,4
image-layer-stack	invariant	CellExtra::{stack_image,set_image,retain_images,images,image}; ImageRef::draw_key	Invariant: images() is sorted non-decreasingly by draw_key and holds ≤ MAX_IMAGE_LAYERS=16 layers; image() = last of images() or None. stack_image(i) inserts i after every layer with key ≤ key(i) (equal keys keep arrival order); if the length exceeds 16 the first (lowest) layer is dropped. set_image(Some i) makes [i] the only layer. retain_images(p) keeps exactly the layers satisfying p in order; an empty stack sets the field to None so has_data no longer counts it.	1	SmallVec in a Box; partition_point; rotate_right/rotate_left; closure parameter; loops	0,1,2,7,11,13
extras-map-basic	contract	CellExtras::{new,get,get_or_create,fresh,set,remove,len,is_empty}	CellExtras refines Ext (§0): get(k)=data[(k.row+row_offset,k.col)], requiring k.row+row_offset ≤ u16::MAX. new() gives Ext=∅. get_or_create(k) returns Ext(k), inserting default() if absent; mutations through the returned &mut land in Ext(k). fresh(k) resets Ext(k) to default(). set(k,e): Ext'=Ext[k↦e] if has_data(e) else Ext∖k. remove(k) deletes k and returns whether it existed. len()=|dom Ext| (stale entries below row_offset excluded).	2	FxHashMap keyed by a struct; &mut returned from a method (entry API); wrapping_add	0,1,2,11
extras-clear-ops	contract	CellExtras::{clear_row,clear_range,clear_rows,clear_rect,clear}; RgbColorRing/ComplexCharRing clear paths	clear_row(r): Ext'=Ext∖row r, RGB ring row r blanked. clear_range(r,a,b): Ext'=Ext∖{(r,c) : a≤c<b} (probing and scanning strategies remove the same keys), RGB ring blanked on [a,min(b,S)). clear_rows(R): Ext'=Ext∖rows R, those RGB rows blanked; empty R is a no-op. clear_rect(R,C): Ext'=Ext∖R×C, both RGB and complex rings blanked on R×C; empty R or C is a no-op. clear(): Ext=∅, row_offset=0, hyperlink bound=0, any_image=false, both rings blank. Every other key, ring cell and value is unchanged, so fg_rgb_for/bg_rgb_for return None inside the cleared set and their old value outside it.	12	HashMap retain with closures; Range<u16>; loops; ring calls	0,1,2,7,13
extras-shifts	contract	CellExtras::{shift_rows_up_by,shift_region_up_by,shift_region_down_by,shift_rect_up_by,shift_rect_down_by,shift_cols_right,shift_cols_left,take_row_range,compact,shift_rings_*,set,get_or_create}	Over Ext; moved values keep every field. shift_rows_up_by(s,n): Ext'(r,c)=Ext(r,c) for r<s; Ext(r+n,c) for r≥s when r+n ≤ u16::MAX; absent otherwise (an overflowing s+n deletes every row ≥ s); n=0 is the identity; with s=0 both rings scroll_up(n). shift_region_up_by(t,b,n): rows outside [t,b] unchanged; for r∈[t,b], Ext'(r,c)=Ext(r+n,c) if r+n ≤ b else absent (t=b deletes row t; an overflowing t+n empties the region). shift_region_down_by(t,b,n): Ext'(r,c)=Ext(r-n,c) if r ≥ t+n else absent; n ≥ b-t+1 empties the region. shift_rect_up_by/shift_rect_down_by(t,b,l,r,n): the same restricted to l≤c≤r. shift_cols_right(row,s,k,m): other rows and columns outside [s,m) unchanged; Ext'(row,c)=Ext(row,c-k) if c≥s+k else absent, entries pushed to ≥m dropped. shift_cols_left: Ext'(row,c)=Ext(row,c+k) if c+k<m else absent. k=0 or n=0 is the identity. Each op shifts the dense rings by the matching ring-model op, ring cells outside the affected rows/rect unchanged. A pending row_offset is folded in first and is invisible here.	64	HashMap drain/rebuild loops; SmallVec; checked and saturating u16 arithmetic; ring calls; &mut self	0,1,2,7,11
extras-render-data	equivalence	CellExtras::render_data_for_cell; CellExtras::{get,complex_codepoint_for,fg_rgb_for,bg_rgb_for}	∀ x,row,col,cell, d=render_data_for_cell(x,row,col,cell), wf=¬uses_style_id(cell) ∧ fg_needs_overflow(cell), wb likewise: d.cell_extra()=get(x,(row,col)) if has_extras(cell) else None; d.complex_char()=complex_codepoint_for(x,row,col) if is_complex(cell) else None (ring first, then the first char of the map entry's complex_char); d.fg_rgb()=fg_rgb_for(x,row,col) if wf else None (ring first, then map); d.bg_rgb() likewise with wb. A cell with none of the four wants returns all None without touching the map.	2	Option chaining; a struct of borrowed Option<&CellExtra>; ring reads	0,1,2,4
extras-uniform-run	contract	CellExtras::set_range_uniform; UniformExtras; CellExtra::set_hyperlink_data	∀ x,row, a<b, vals: if vals carries only fg/bg RGB (no link, no underline, ext=0) and at least one RGB, the effect is set_rgb_ring_range(row,a,b,..) on the ring only and Ext is unchanged. Otherwise ∀ c∈[a,b), Ext'(row,c) is a fresh CellExtra holding exactly vals' fields (link=(url,id) when vals.hyperlink is Some); other keys unchanged; the hyperlink bound changes by (b-a if linked) minus the replaced linked entries, then enforce_hyperlink_limit runs. a≥b is a no-op. Observationally equal to the per-cell loop (fresh(k) then set_*); sharing one Arc<HyperlinkData> is unobservable (a later per-cell edit changes only that column).	1	loop over a column range; Arc sharing with copy-on-write; HashMap entry API	0,1,2,7
hyperlink-bound-invariant	invariant	CellExtras::{get_or_create,fresh,set,remove,set_range_uniform,clear_*,shift_*,compact,clear,enforce_hyperlink_limit}; CellExtras::hyperlink_upper_bound (observer)	∀ CellExtras reachable from new() by any sequence of public/crate ops, including mutations through the &mut returned by get_or_create/fresh: hyperlink_upper_bound(x) ≥ L(x). clear() sets both to 0; repainting an already-linked run with set_range_uniform leaves the bound unchanged; remove of a linked entry refunds one.	1	ghost-counter invariant across many &mut self methods, including writes through a returned &mut CellExtra (needs prophecy or an entry view)	0,1,2,5,7
hyperlink-limit	contract	CellExtras::{enforce_hyperlink_limit,enforce_hyperlink_limit_cold,apply_offset_and_shift_only}; MAX_HYPERLINK_ENTRIES	MAX=10_000, LOW=7_500. If L(x) ≤ MAX, enforce_hyperlink_limit leaves Ext unchanged (it may only lower the bound to L(x)). If L(x) > MAX, afterwards L=LOW exactly: the links removed are those of the L(x)-LOW entries smallest in (row,col) order after folding row_offset; each such entry loses only its link and is deleted iff it has no other data; all other entries untouched. Hence the map never holds more than MAX links after any op ending with this call.	5	Vec scratch buffer; select_nth_unstable_by with a closure; HashMap get_mut/remove in a loop	0,1,2,7,13
hyperlink-walk-cost	cost	CellExtras::enforce_hyperlink_limit	enforce_hyperlink_limit performs the cold O(|map|) walk only when hyperlink_upper_bound > MAX and |map| > MAX. After a walk that finds L ≤ MAX the bound equals L, so later calls with no new charges walk nothing; a map of ≤ MAX entries never walks however inflated the bound. State over a ghost walk counter (today count_hyperlink_walk_ops).	2	ghost or instrumentation counter; short-circuit condition; loop count	0,1,2,7
extras-op-cost	cost	CellExtras::{shift_rows_up_by,apply_offset_and_shift,shift_region_up_by,shift_region_down_by,shift_cols_right,shift_cols_left,clear_row,clear_rows}; COMPACT_THRESHOLD; Grid::{scroll_up,erase_to_end_of_screen,erase_rect}; test_counters::{count_extras_shift_ops,count_extras_clear_ops}	Over ghost counters EXTRAS_SHIFT_OPS/EXTRAS_CLEAR_OPS, for a map of E entries: shift_rows_up_by(0,n) with row_offset+n<256 touches no entry and sets row_offset+=n; the call that would reach ≥256 (or overflow) compacts once in O(E) and leaves row_offset=0; shift_rows_up_by(s>0,n) costs O(E). Grid::scroll_up(n) adds 0 shift ops for any n and population (including 100 successive scroll_up(1)). shift_region_up_by/down_by add exactly E, independent of b-t and n. shift_cols_right/left(row,start,count,max) add ≤ min(E,max-start), independent of count. clear_row and clear_rows(range) add exactly E, independent of |range|. erase_to_end_of_screen and erase_rect add O(E) (a constant number of batch passes). Content always follows extras-shifts/extras-clear-ops (e.g. once every row scrolls off, len()=0).	17	thread_local counters as ghost cost; HashMap iteration counts; drain/rebuild loops	0,1,2,5,7,8
extras-capacity-policy	contract	CellExtras::{maybe_shrink,clear,clear_row,clear_range,clear_rows,clear_rect,retain_rows_below,retain_cols_below}	After any clear_* or retain_* call: if the map's capacity just before maybe_shrink satisfied cap > 64 ∧ cap > 4·len, the capacity is reduced (shrink_to_fit), else unchanged. clear() replaces the map with an unallocated one, so its capacity ends strictly below any capacity ≥ 200 it had. Content per extras-clear-ops.	3	HashMap capacity semantics (shrink_to_fit promises only 'as much as possible'); needs a hashbrown capacity model	0,1,2
alloc-slice-contract	contract	PageStore::{alloc_slice,alloc_slice_impl,alloc_page}; page_store::align_up; Page::new; PageSlice::{as_slice,as_mut_slice,len_u16,page_id}	∀ store, T, len with len·size_of T ≤ PAGE_SIZE: s=alloc_slice::<T>(len) has |s|=len, s.as_ptr() aligned to align_of T, and [ptr,ptr+len·size_of T) inside one page and disjoint from every live slice allocated earlier from this store. Its offset is align_up(next_offset,align) on the current page if it fits, else 0 on a new page (free list first); next_offset'=offset+len·size_of T, so len=0 consumes no capacity beyond alignment. On a fresh or preheated page every byte of s is 0. len·size_of T > PAGE_SIZE panics. as_slice and as_mut_slice name the same range.	8	generic T (size/align), raw pointers, NonNull, unsafe ptr add/cast/from_raw_parts, bit-mask align_up, Vec<Box<Page>>, UnsafeCell	0,1,2,3,8,11
page-store-row-separation	invariant	PageStore::{alloc_cells,alloc_slice}; PageSlice::{as_slice,as_mut_slice}; Row::{new,resize,write_char*,get_unchecked_mut}; Grid::row_mut	Separation: ∀ PageStore P and distinct live rows a,b ∈ Live(P), Region(a) ∩ Region(b) = ∅, across page boundaries and after any resize (which moves a row to a fresh region). Frame: a mutation through one row (write*, get_unchecked_mut, resize), a further alloc_slice, or growth of the pages Vec leaves every other live slice's contents unchanged (pages are Box<Page> that never move or free while the store lives). At Grid level, ∀ r1≠r2 < rows(g), a mutation through g.row_mut(r1) leaves g.row(r2)'s cells unchanged.	8	raw-pointer arena (PageSlice), unsafe, separation-logic frame over heap regions, Box stability under Vec growth, Vec<Row> in GridStorage	0,1,2,8,16
page-pool-reuse-and-memory	contract	PageStore::{new,with_capacity,preheat,alloc_page,alloc_slice,shrink_to_fit,total_memory}	preheat(n) pushes n all-zero pages to the free list; with_capacity(n)=new() then preheat(n). An alloc_slice that needs a new page pops the free list when non-empty and allocates a fresh page only when it is empty; an allocation that fits the current page takes no page. shrink_to_fit empties the free list. total_memory() = (|pages|+|free_pages|)·PAGE_SIZE + recycled_cells.capacity()·size_of::<PageSlice<Cell>>(), saturating. (PoolStats is written but never read in production; its assertions are not part of the spec.)	6	Vec push/pop/clear/shrink_to_fit, Box<Page>, saturating usize, &mut self	0,1,2,3
recycled-row-cells	contract	PageStore::{alloc_cells,recycle_cells,total_memory}; Row::{new,recycle}	recycle_cells(s) pushes s; alloc_cells(cols) removes and returns the last recycled slice with len=cols if one exists, else alloc_slice(cols); a slice of another width is never consumed, and a recycled slice is handed out at most once (unique ownership). Row::new fills whatever it gets with EMPTY, so a row built from a recycled slice has len 0 and only EMPTY cells; allocations counts only the fresh path; total_memory grows with the recycled pool's capacity.	1	Vec rposition/swap_remove, move of non-Clone PageSlice, unsafe fn contracts	0,1,2,3,8
page-send-sync-sound	other	unsafe impl Send/Sync for Page; unsafe impl<T: Send> Send / <T: Sync> Sync for PageSlice<T>; Page::data_ptr; PageSlice::as_slice	The unsafe Send/Sync impls are sound: moving a Page or PageSlice<T> (T: Send) to another thread preserves its contents, and concurrent shared reads of an Arc<Page>/Arc<PageSlice<T>> (T: Sync) through data_ptr/as_slice are data-race free and see the values written before the share (happens-before via spawn/Arc).	4	unsafe impl Send/Sync, raw pointers through UnsafeCell, thread::spawn, Arc, closures	0,8,13
row-new	contract	Row::new; PageStore::{new,preheat,alloc_cells}	∀ cols:u16 and every PageStore P (fresh, preheated, or holding pages freed by dropped or recycled rows) that outlives the row (unsafe precondition): r=Row::new(cols,&mut P) has cols(r)=cols, len(r)=0, Cells(r)=[EMPTY;cols], get(r,c)=None for c≥cols, flags(r)=DIRTY (so line_size=SingleWidth, ¬is_wrapped, ¬HAS_WIDE_CHARS, ¬EARLY_WRAP_HOLE) and WideInv(r). Reused or lazily zeroed page memory never leaks stale cells.	4	unsafe fn with a lifetime-style precondition; arena free list and lazy zeroing; slice fill; List model of PageSlice	0,1,2,3,5,7,8
row-get-unchecked	equivalence	Row::{get,get_unchecked,get_unchecked_mut}	requires c < cols(r). *get_unchecked(r,c) = *get(r,c).unwrap(); get(r,c)=None ⇔ c ≥ cols(r). A write v through get_unchecked_mut(r,c) gives Cells(r')=Cells(r)[c:=v] and changes nothing outside Region(r).	5	unsafe fn with a requires clause, slice::get_unchecked(_mut), &mut Cell returned to the caller (prophecy)	0,1,2,5,8
orphan-fixup-primitives	contract	Row::{orphaned_wide_halves,fixup_wide_char_overwrite,fixup_wide_char_write,fixup_wide_chars_in_range,is_cell_wide_continuation}	orphaned_wide_halves(s,e) with e'=min(e,cols): s≥e' ⇒ [None,None]; else [cont(s) ? Some(s-1) : None, (e'<cols ∧ WIDE(cells[e'-1])) ? Some(e') : None]. fixup_wide_char_overwrite(c,..) blanks exactly Orph(c,c+1) to EMPTY and returns whether c+1 was blanked; fixup_wide_char_write(c,..) blanks exactly Orph(c,c+2) and returns whether c+2 was blanked; fixup_wide_chars_in_range(s,n) is a no-op returning false without HAS_WIDE_CHARS, else blanks exactly Orph(s,s+n) and returns whether its right half was blanked. Cells inside the span and all others unchanged.	3	Row via PageSlice deref, slice indexing, CellFlags bit tests, [Option<u16>;2], bool returns	0,1,2,3,5,8
row-set	contract	Row::set; Row::note_write_through	set(col,cell): col≥cols ⇒ false, row unchanged. Else true, cells[col]:=cell, len'=(col≥len ∧ ¬cell.is_empty() ? col+1 : len) (no overflow: cols ≤ PAGE_SIZE/size_of Cell), flags'=flags∪DIRTY∪(WIDE∈cell ? HAS_WIDE_CHARS : ∅)∪(USES_STYLE_ID∈cell ? HAS_STYLE_ID : ∅), EARLY_WRAP_HOLE cleared iff col=cols-1; every other cell unchanged.	4	Row via PageSlice deref, slice indexing, bit tests, u16/usize arithmetic	0,1,2,3,5,8
row-narrow-write	contract	Row::{write_char,write_char_packed,write_char_styled,settle_len_after_write,note_write_through,fixup_wide_char_overwrite,orphaned_wide_halves}	For w ∈ {write_char(col,ch), write_char_packed(col,ch,colors,f), write_char_styled(col,ch,fg,bg,f)=write_char_packed(col,ch,convert_colors(fg,bg),f)} on a row with WideInv: col≥cols ⇒ false and row unchanged (write_char's pre-fixup reads go through get, so they cannot go out of bounds). col<cols ⇒ true and (a) cells[col] := the old cell with set_char(ch) for write_char, from_raw_parts(ch ≤ MAX_DIRECT_CODEPOINT ? ch : U+FFFD, colors, f) for the packed forms; (b) if HAS_WIDE_CHARS each column of Orph(col,col+1) (pre-state) := EMPTY; (c) every other cell unchanged (col-1 touched only when cont(col)); (d) len'=settle(len,col+1,h), h = column col+1 was blanked; (e) DIRTY set, EARLY_WRAP_HOLE cleared iff col=cols-1, other flags unchanged; WideInv holds after.	34	Row via PageSlice deref (unsafe from_raw_parts), get_unchecked(_mut), bit tests, char→u32 cast	0,1,2,3,4,5,8
row-wide-write	contract	Row::{write_wide_char_packed,write_wide_char_packed_no_fixup,fixup_wide_char_write,settle_len_after_wide_run,mark_has_wide_chars}; CellFlags::wide_continuation_of	write_wide_char_packed(col,ch,colors,f): col+1≥cols ⇒ false, row unchanged (a 1-column row never holds a wide char). Else true and: each column of Orph(col,col+2) (pre-state) := EMPTY; cells[col]:=from_raw_parts(cp(ch),colors,f∪WIDE) with cp(ch)=ch if ≤ MAX_DIRECT_CODEPOINT else U+FFFD; cells[col+1]:=from_raw_parts(' ',colors,f.wide_continuation_of()); all other cells unchanged; len'=settle(len,col+2,h), h = column col+2 blanked; flags'=flags∪DIRTY∪HAS_WIDE_CHARS, EARLY_WRAP_HOLE cleared iff col+2=cols; WideInv holds after. write_wide_char_packed_no_fixup (requires col+1<cols) writes the same two cells and nothing else; settle_len_after_wide_run(e,h) sets len'=settle(len,e,h) and clears EARLY_WRAP_HOLE iff e≥cols. (write_wide_char is a #[cfg(test)] delegation.)	19	Row via PageSlice deref, unsafe get_unchecked_mut pair writes, unsafe fn with caller precondition, bit tests	0,1,2,3,4,5,8
fill-cell-run	contract	Row::fill_cell_run; Row::fixup_wide_chars_in_range	fill_cell_run(col,n,t): n=0 ∨ col≥cols ⇒ returns 0, row unchanged. Else with e=min(col+n,cols): if HAS_WIDE_CHARS each column of Orph(col,e) (pre-state) := EMPTY; cells[col..e]:=t; others unchanged; returns e-col; len'=settle(len,e,h), h = column e blanked (written blanks count exactly as the same blanks written one at a time); DIRTY set; EARLY_WRAP_HOLE cleared iff e=cols.	12	Row via PageSlice deref, slice::fill, bit tests	0,1,2,3,5,8
cells-mut-with-fixup	contract	Row::{cells_mut_with_fixup,update_len}	cells_mut_with_fixup(s,n): s>cols ⇒ None, row unchanged (no overflow at s=u16::MAX). Else with e=min(s+n,cols) (saturating): if HAS_WIDE_CHARS each column of Orph(s,e) := EMPTY; returns Some(cells[s..e]) (possibly empty); when e>s DIRTY set and EARLY_WRAP_HOLE cleared iff e=cols; len'=(h ∧ e+1=len ? e : len), h = column e blanked; nothing else changes. update_len(e): len'=max(len,min(e,cols)) (grow-only, clamped).	10	Row via PageSlice deref, returned &mut [Cell] tied to &mut self, bit tests	0,1,2,3,5,8
ich-insert-chars-fill	contract	Row::insert_chars_fill; Row::recalculate_len_up_to	insert_chars_fill(col,n,f) (insert_chars is the #[cfg(test)] EMPTY delegation): n=0 ∨ col≥cols ⇒ unchanged. Else with k=cols-n (saturating): if cont(col), cells[col-1] and cells[col] := f first; if 0<k<cols ∧ WIDE(cells[k-1]), cells[k-1]:=f first; then cells'[j]=cells[j] for j<col, f for j∈[col,min(col+n,cols)), cells'[j+n]=cells[j] for j∈[col,k) (shifted past cols discarded); DIRTY; EARLY_WRAP_HOLE cleared. len': old len=0 ∨ col≥old len ⇒ (f empty ? len : tight over [0,min(col+n,cols))); else old len+n if ≤ cols, else (cells'[cols-1] empty ? tight over the row : cols). A protected non-continuation cell is shifted intact; col-1 is touched only when cont(col).	6	Row via PageSlice deref, copy_within (memmove), bit tests	0,1,2,3,5,7,8
dch-delete-chars-fill	contract	Row::delete_chars_fill; Row::recalculate_len_up_to	delete_chars_fill(col,n,f): n=0 ∨ col≥cols ⇒ unchanged. Else with ld=min(col+n-1,cols-1), src=min(col+n,cols): if cont(col), cells[col-1]:=f; if WIDE(cells[ld]) ∧ src<cols, cells[src]:=f; then cells'[j]=cells[j] for j<col, cells'[col+i]=cells[src+i] for i<cols-src, f for j ≥ col+(cols-src); DIRTY; EARLY_WRAP_HOLE cleared. len': old len=0 ∨ col≥old len ⇒ (f empty ? len : tight over the row); f non-empty ⇒ tight over the row; else if col+n<old len: cand=old len-n, tightened to tight over [0,cand) when cells'[cand-1] is empty; else tight over [0,col).	8	Row via PageSlice deref, copy_within, bit tests	0,1,2,3,5,7,8
ech-erase-chars-with	contract	Row::erase_chars_with	erase_chars_with(col,n,f) (erase_chars is the EMPTY delegation): n=0 ∨ col≥cols ⇒ unchanged. Else with e=min(col+n,cols): if cont(col), cells[col-1]:=f; if e<cols ∧ WIDE(cells[e-1]), cells[e]:=f; cells[col..e]:=f; no shift, all other cells unchanged; DIRTY; EARLY_WRAP_HOLE unchanged. len': f.colors≠EMPTY.colors ⇒ max(e,old len); else if col<old len ∧ cells'[old len-1] empty ⇒ tight over [0,old len); else unchanged (erasing the middle keeps len).	5	Row via PageSlice deref, slice loop, bit tests	0,1,2,3,5,7,8
bounded-ich-dch	contract	Row::{insert_chars_bounded_fill,delete_chars_bounded_fill}	With rb=min(right_bound,cols): n=0 ∨ col≥rb ⇒ unchanged. insert_chars_bounded_fill(col,n,rb,f): if cont(col) both halves := f; with k=rb-n: if col<k<rb ∧ WIDE(cells[k-1]), cells[k-1]:=f; if rb<cols ∧ WIDE(cells[rb-1]), cells[rb]:=f; then the ICH shift of ich-insert-chars-fill restricted to [col,rb). delete_chars_bounded_fill: the DCH shift of dch-delete-chars-fill restricted to [col,rb) with the same fixups. Both: DIRTY, EARLY_WRAP_HOLE cleared iff rb=cols, len'=tight over the row; col-1 touched only when cont(col), so a protected cell left of the margin survives and no stale bit-10 continuation shifts into the region.	1	Row via PageSlice deref, copy_within, bit tests	0,1,2,3,5,7,8
clear-range-with-orphan	contract	Row::{clear_range_with_orphan,clear_range_with}	clear_range_with_orphan(s,e,f,o) requires o∈{EMPTY,f}. With e'=min(e,cols): s≥e' ⇒ unchanged, returns s..s. Else: if s>0 ∧ WIDE(cells[s-1]), cells[s-1]:=o; if e'<cols ∧ WIDE(cells[e'-1]), cells[e']:=o; cells[s..e']:=f; others unchanged; DIRTY; returns the widened range (s-1 if left orphan else s)..(e'+1 if right orphan else e'). len': f non-empty ⇒ (right orphan ∧ o non-empty ? max(e'+1,old) : right orphan ∧ old=e'+1 ? e' : max(e',old)); f empty ∧ s<old ∧ (e'≥old ∨ (right orphan ∧ old=e'+1)) ⇒ tight over [0,s); else unchanged. clear_range_with(s,e,f)=clear_range_with_orphan(s,e,f,EMPTY), and additionally clears EARLY_WRAP_HOLE when e≥cols and f is not a wrap-hole blank.	10	Row via PageSlice deref, Range<u16> result, slice::fill, bit tests	0,1,2,3,5,8
selective-erase-decsca	contract	Row::{selective_clear_range,selective_wipe_range,selective_clear,is_cell_protected}	selective_clear_range(s,e), e'=min(e,cols), s<e': (1) if s>0 ∧ bit10(cells[s]) ∧ WIDE(cells[s-1]) ∧ ¬is_cell_protected(s-1), cells[s-1],cells[s] := EMPTY; (2) scanning c=s..e' left to right, every c with ¬is_cell_protected(c) (on the current state) becomes EMPTY, together with cells[c+1] when WIDE(cells[c]) ∧ c+1<cols; protected cells unchanged; nothing outside [s-1,e'] changes; if anything was erased: DIRTY, and s<old len ∧ cells'[old len-1] empty ⇒ len'=tight over [0,old len). s≥e' ⇒ unchanged. selective_clear() = the same scan over [0,cols) without step (1). selective_wipe_range(s,e,w) is the same scan but each erased cell gets set_char(' ') (SGR flags, colours, style id kept) and its column is appended to w.	7	Row via PageSlice deref, &mut Vec<u16> out-parameter, a loop whose guard reads cells it mutates	0,1,2,3,5,7,8
bit10-locality	contract	Row::{is_cell_wide_continuation,is_cell_protected,write_char,write_char_packed,write_wide_char_packed,fill_cell_run,insert_chars_fill,delete_chars_fill,erase_chars_with,insert_chars_bounded_fill,delete_chars_bounded_fill}	With cont and is_cell_protected(c) ⇔ c<cols ∧ bit10(cells[c]) ∧ (WIDE(cells[c]) ∨ ¬(c>0 ∧ WIDE(cells[c-1])) ∨ PROTECTED(cells[c-1])): for every listed op acting on a span starting at col, ∀ j<col-1 cells'[j]=cells[j], and cells'[col-1]≠cells[col-1] ⇒ cont(col) held in the pre-state. A DECSCA-protected non-continuation cell moved by an ICH/DCH shift arrives bit-for-bit intact. (A frame corollary of the per-op contracts.)	6	Row via PageSlice deref, bit tests	0,1,2,3,5,8
fixup-wide-boundary	contract	Row::fixup_wide_boundary	fixup_wide_boundary(l,r,cols,clear). Left (0<l<|cells|), pw=WIDE(cells[l-1]), cc=bit10(cells[l]): pw ∧ ¬cc ⇒ cells[l-1]:=EMPTY; clear ∧ cc ∧ ¬pw ∧ char(cells[l])=' ' ⇒ cells[l]:=EMPTY. Right (r+1<min(cols,|cells|)), cw=WIDE(cells[r]), nc=bit10(cells[r+1]): cw ∧ ¬nc ⇒ cells[r]:=EMPTY; clear ∧ nc ∧ ¬cw ∧ char(cells[r+1])=' ' ⇒ cells[r+1]:=EMPTY. No other cell changes. So a protected visible cell is never cleared, a protected space survives when clear=false, and a dangling WIDE head is always cleared.	4	Row via PageSlice deref, bit tests	0,1,2,3,5,8
row-resize	contract	Row::resize; PageStore::alloc_slice; Row::recalculate_len_up_to	resize(n,P), P outliving the row (unsafe precondition): n=cols ⇒ unchanged. Else cells' is a fresh PageSlice of n cells with cells'[i]=cells[i] for i<min(cols,n) (flags and colours kept) and EMPTY beyond; len'=min(len,n); if n<cols ∧ WIDE(cells'[n-1]), cells'[n-1]:=EMPTY and len'=tight over the row (a bit-10 cell at 0 is never cleared); flags'=(flags∪DIRTY)∖EARLY_WRAP_HOLE. Columns ≥n are gone (get returns None). Sequences of resizes compose by these rules, and a later write lands at the new geometry (no stale PageSlice).	10	unsafe fn (arena outlives row), arena re-allocation, copy_from_slice of a prefix, &mut self PageSlice replacement	0,1,2,3,5,7,8
row-clear-erase-reset	contract	Row::{clear,erase_with,reset_with}	clear(): every cell EMPTY, len'=0, flags'=DIRTY. erase_with(f): every cell f, len'=(f.colors=EMPTY.colors ? 0 : cols), flags'=(flags∩LINE_ATTRIBUTES)∪DIRTY. reset_with(f): cells and len as erase_with, flags'=DIRTY.	1	Row via PageSlice deref, slice::fill, bitflags	0,1,2,3,5,8
row-line-size	contract	Row::{set_line_size,line_size}	∀ s: line_size() after set_line_size(s) = s, with flag image SingleWidth↦∅, DoubleWidth↦{DOUBLE_WIDTH}, DoubleHeightTop↦{DOUBLE_WIDTH,DOUBLE_HEIGHT_TOP}, DoubleHeightBottom↦{DOUBLE_WIDTH,DOUBLE_HEIGHT_BOTTOM} (other line-size bits removed, other flags kept, DIRTY set). For s≠SingleWidth: cells[max(cols/2,1)..]:=EMPTY, cells below unchanged, len'=tight over [0,max(cols/2,1)) when that start was < old len. A new row is SingleWidth.	5	Row via PageSlice deref, enum match, slice loop, u16 division	0,1,2,3,5,8
row-copy-from	contract	Row::copy_from	For rows d and s in distinct regions, k=min(cols(d),cols(s)): Cells(d')[..k]=Cells(s)[..k], Cells(d')[k..]=Cells(d)[k..], len(d')=min(len(s),cols(d)), flags(d')=flags(s)∪DIRTY (so is_wrapped is copied), s unchanged. Two textually identical bodies exist today (#[cfg(miri)] pub and #[cfg(not(miri))] pub(crate)); the proof targets the production one and the miri copy should go.	3	&mut self plus &other (two PageSlice derefs), copy_from_slice	0,1,2,3,5,8
row-wrapped-flag	contract	Row::{set_wrapped,is_wrapped}	∀ b: after set_wrapped(b), is_wrapped()=b and every other flag bit, all cells and len are unchanged.	1	&mut self, u8 bitflags newtype	0,1,2,3
row-display	contract	<Row as Display>::fmt	row.to_string() = concat over cells[..len] of (c.char()='\0' ? ' ' : c.char()), skipping every cell with raw bit 10 set (Cell::is_wide_continuation). As written this also drops DECSCA-protected cells (§5); the intended spec skips only cont(c).	1	Formatter/String writes, char mapping, slice iteration through PageSlice deref	0,1,2,3,6,7,8
early-wrap-hole-lifecycle	invariant	Row::{has_early_wrap_hole,set_early_wrap_hole,note_write_through} and every Row writer	has_early_wrap_hole() ⇔ EARLY_WRAP_HOLE∈flags ∧ is_wrap_hole_blank(cells[cols-1]). The flag is cleared by every op whose written span reaches cols (set, write_char*, write_wide_char_packed, fill_cell_run, cells_mut_with_fixup, settle_len_after_wide_run with end ≥ cols; insert/delete_chars_fill always; bounded shifts iff rb=cols; clear_range_with iff e≥cols and the fill is not a wrap-hole blank) and by resize, clear, erase_with, reset_with. It is kept by erase_chars_with, a blank clear_range_with, bounded shifts with rb<cols and writes ending before the last cell. A write that returns false changes nothing.	2	Row via PageSlice deref, bit tests; a cross-op flag invariant	0,1,2,3,5,8
row-unsafe-ub-free	no-panic	Row::{new,resize,get_unchecked,get_unchecked_mut,write_char_packed,write_wide_char_packed}; PageStore::*; PageSlice::*; Grid::resize	Under each unsafe fn's documented precondition (pages outlives every row allocated from it; col<cols for the unchecked accessors), every unsafe block in Row, PageSlice and PageStore is UB-free: in-bounds, aligned, initialized reads, no use-after-free across free-list reuse, no aliasing &mut. The safe API (write*, get, resize through Grid, Grid::resize reflow) neither panics nor reaches UB for any input. This is the property Miri samples; proving it retires the STOCK EXCEPTION nightly-miri lane for these paths.	2	unsafe blocks, raw pointers, arena allocator, get_unchecked, Vec, the &mut aliasing model	0,1,2,8,16
tab-default-stops	table-property	is_default_tab_stop; GridCursorState::{default_tab_stops,reset_tab_stops}; Grid::{new,reset_tab_stops}	∀ col: is_default_tab_stop(col) ⇔ col>0 ∧ col%8=0. ∀ rows,cols: Grid::new(rows,cols).tab_stops() = [is_default_tab_stop(i) | i<cols] and tab_defaults_suppressed()=false. After g.reset_tab_stops() (and GridCursorState::reset_tab_stops(n)), tab_stops() = [is_default_tab_stop(i) | i<cols] (length exactly cols, any wider array dropped) and suppressed=false.	3	const fn with usize is_multiple_of, Vec<bool> map/collect	0,1,3,7
tab-stop-set-clear	contract	Grid::{set_tab_stop,clear_tab_stop,clear_all_tab_stops}; GridCursorState::{set_tab_stop_at,clear_tab_stop_at,clear_all_tab_stops}	Let c=cursor.col, S=tab_stops. set_tab_stop: S'=(c<|S| ? S[c:=true] : S). clear_tab_stop: S'=(c<|S| ? S[c:=false] : S). Both keep suppressed. clear_all_tab_stops: |S'|=|S| ∧ ∀i S'[i]=false ∧ tab_defaults_suppressed'=true. All three leave the cursor and every other field unchanged. (storage.rs tests read through the #[cfg(test)] is_tab_stop; retarget them to tab_stops().)	4	&mut self, Vec<bool> index with a bounds guard, Vec::fill	0,1,2,5
ht-next-stop	contract	Grid::{tab,tab_margin,tab_n_margin}; GridStorage::max_col_for_row	r=cursor.row, c=cursor.col, m=max_col_for_row(r), S=tab_stops (|S|≥m+1). tab(): col'=min{j | c<j≤m ∧ S[j]} if non-empty, else m; row, pending_wrap and S unchanged. tab_margin(false)=tab(). tab_margin(true) with (L,R)=margins, B=(L≤c≤R ? min(R,m) : m): col'=min{j | c<j≤B ∧ S[j]} else B; so c≤B ⇒ col'≥c (a forward tab never moves backward, in particular when c>R). tab_n_margin(n,l) = n-fold tab_margin(l). (tab_n is a #[cfg(test)] alias; its tests retarget to tab_n_margin.)	6	&mut self, for loop with early break over Vec<bool>, saturating_add, min, usize/u16 conversion	0,1,2,3,5,7
cbt-prev-stop	contract	Grid::{back_tab,back_tab_margin,back_tab_n_margin}	m=max_col_for_row(cursor.row), c=min(cursor.col,m), S=tab_stops. back_tab(): c=0 ⇒ unchanged; else col'=max{j<c | S[j]} if non-empty, else 0. back_tab_margin(l,o): ¬(l∧o) ⇒ back_tab(); l∧o with L=margins.left: c≤L ⇒ unchanged (never dragged forward onto the margin); else col'=max{j∈[L,c) | S[j]} else L. Always col'≤cursor.col; pending_wrap and S unchanged. back_tab_n_margin(n,l,o) = n-fold. (back_tab_n is a #[cfg(test)] alias.)	8	&mut self, reverse range loop with early return, min	0,1,2,3,5,7
tab-stops-resize-grow-only	contract	GridStorage::resize_viewport_state; Grid::resize	∀ g, new_rows, new_cols≠cols, S=tab_stops, n=|S|: new_cols≤n ⇒ S'=S (the array never shrinks, so stops beyond a narrow width survive a later grow); new_cols>n ⇒ S'=S ++ [(suppressed ? false : is_default_tab_stop(j)) | j∈[n,new_cols)]; suppressed unchanged. Corollary: after clear_all_tab_stops and any HTS, no widen creates a stop; with suppressed=false a widen seeds every-8 stops in the new columns.	2	&mut self, Vec::resize, loop over the new suffix; Grid::resize frames every other path	0,1,2,3,5,7
restore-tab-stops-admission	refinement	Grid::{restore_tab_stops,tab_stops}; aterm_spec::derive::tab_stop_handoff_model	restore_tab_stops(s,d): cols ≤ |s| ≤ MAX_GRID_COLS ⇒ tab_stops'=s ∧ suppressed'=d; otherwise the grid is unchanged (undersize and protocol-oversize projections rejected). With tab-stops-resize-grow-only and ht-next-stop, the projection (admitted, rejected, restored_len, stops) of shrink→capture→restore→grow→HT refines tab_stop_handoff_model and satisfies AdmissionIsCoveringAndBounded; the Buggy=1 prefix-copy restore violates it (negative control).	1	&mut self, &[bool] length comparisons, Vec clear/extend_from_slice, model-to-code projection	0,1,2,10
grid-construct	contract	Grid::{new,with_scrollback,with_tiered_scrollback,default}; Grid::assert_invariants (observer)	∀ rows,cols:u16, max, sb, g ∈ {new(rows,cols), with_scrollback(rows,cols,max), with_tiered_scrollback(rows,cols,max,sb)}: rows(g)=clamp(rows,1,MAX_GRID_ROWS), cols(g)=clamp(cols,1,MAX_GRID_COLS); Inv(g); cursor (0,0), pending_wrap=false, scroll region and margins full, display_offset=0, every screen row blank and unwrapped (len 0), damage Full (needs_full_redraw()). History is empty except for with_tiered_scrollback, whose history is sb's lines in order. default()=new(24,80).	9	large struct literal; Vec push loop; unsafe Row::new over a page arena; clamp	0,1,2,3,7,8
grid-type-invariant	invariant	Grid::assert_invariants (Inv); every pub &mut self method of Grid; GridStorage::push_ring_extras; ring growth and reuse (grow_scrollback_ring, reuse_scrolled_rows)	Inv(g) (§0, with RingExtrasAligned) holds for every constructor and is preserved by every public mutator with any arguments, which never panics: writes, line_feed and reverse_line_feed inside and outside a region, scroll_up(n) (growth, reuse, batch, n > capacity), the erase family, erase_scrollback on an empty history, and resize/reflow in any mode (row and column shrink and grow, extreme shrink, while scrolled back). Sizes are clamped to [1,MAX_GRID_ROWS]×[1,MAX_GRID_COLS]. Corollaries: after resize(r,c) cursor.row<r and cursor.col<c (c=1 ⇒ col 0, r=1 ⇒ row 0); the default region is [0,rows-1]; push_ring_extras(e) appends exactly one entry, stored as None iff e.is_empty().	21	&mut self over GridStorage (Vec<Row> ring, modular ring_head), unsafe page-backed rows, HashMap extras, running sums, loops; an invariant framed across ~40 mutators	0,1,2,3,4,5,7,8
row-access-bounds	contract	GridStorage::{row,row_mut,row_mut_with_effective_cols,row_index}; Grid::row	∀ g∈Reach, v:u16, d=display_offset: row(v).is_some() ⇔ row_mut(v).is_some() ⇔ row_mut_with_effective_cols(v).is_some() ⇔ v<visible_rows ∧ d ≤ (total_lines-visible_rows)+v, i.e. the row is ring-resident; otherwise None (a row in the lazy/tiered history, never an underflowed index or a panic). When Some, row_index(v) = (ring_head+total_lines-visible_rows+v-d) mod |rows| < |rows| (this discharges the get_unchecked in row_mut_with_effective_cols). At d=0, row(v) is Some for every v<rows, also after any resize.	8	&self/&mut self, modular usize arithmetic, Option, unsafe get_unchecked(_mut)	0,1,2,3,8
column-geometry	contract	GridStorage::{effective_cols_for_row,max_col_for_row,clamp_col_for_row,row_is_double_width}	∀ g∈Reach (where any_double_width holds whenever some row is double width or double height) and row r: EC(r)=effective_cols_for_row(r)=max(1,cols/2) if line_size(r) ∈ {DoubleWidth, DoubleHeightTop, DoubleHeightBottom}, else max(cols,1); MC(r)=max_col_for_row(r)=EC(r)-1; clamp_col_for_row(r,c)=min(c,MC(r)).	7	&self, enum match on LineSize, u16 division, Option map	0,2,3
row-u16-saturating	contract	row_u16	∀ x:usize: row_u16(x) = min(x,65535) as u16.	2	usize→u16 try_into + unwrap_or	0,3
ring-row-shift-permutation	contract	GridStorage::{shift_visible_rows_up,shift_visible_rows_down}	Requires display_offset=0. ∀ t≤b<visible_rows, n: n=0 or t+n>b ⇒ identity on rows. Otherwise shift_visible_rows_up permutes Row structs so visible row i holds old row i+n for t≤i≤b-n and rows b-n+1..=b hold old rows t..t+n-1; shift_visible_rows_down mirrors. Rows outside [t,b] unchanged; the multiset of (cell pointer, cells_len, page_id) over all rows is unchanged; the PageStore allocation count is unchanged.	5	&mut self, Vec::swap in a loop with modular indices, permutation loop invariant	0,1,2,3,5,7
region-and-margin-setters	contract	GridCursorState::{set_scroll_region,reset_scroll_region,set_horizontal_margins,reset_horizontal_margins}; GridStorage::set_horizontal_margins	∀ R rows, C cols, t,b,l,r: set_scroll_region(t,b) sets (t,b') with b'=min(b,R-1) if t<b', else ScrollRegion::full(R) (top=bottom and inverted ranges fall back to full, per VT510); reset_scroll_region sets full(R). set_horizontal_margins(l,r) sets (l,r) if l<r<C, else full(C); reset sets full(C). Afterwards has_horizontal_margins = ¬margins.is_full(C).	7	&mut self on Copy structs, u16 comparisons	0,2,3
cursor-set-clamp	contract	Grid::set_cursor; GridStorage::{clamp_col_for_row,set_cursor_position}; Grid::move_cursor_to (cfg(test) alias)	∀ g∈Reach, row,col: after set_cursor(row,col): r'=min(row,R-1), c'=min(col,MC(r')), pw'=false, everything else unchanged. move_cursor_to is the same call. No cursor motion changes content.	9	&mut self over GridStorage, u16 min/saturating_sub, calls	0,2,3,4,6
cursor-vertical-margins	contract	Grid::{cursor_up,cursor_down,line_position_relative}	∀ g∈Reach, n:u16: cursor_up(n): r'=max(r -sat n, r≥top ? top : 0); cursor_down(n): r'=min(r +sat n, r≤bot ? bot : R-1); line_position_relative(n) (VPR): r'=min(r +sat n, R-1), ignoring the region. All three: pw'=false, c'=(any_double_width ? clamp_col_for_row(r',c) : c), cells unchanged; n=0 ⇒ r'=r. (This is stronger than the ENSURES doc on cursor_up/down; see §5.)	15	&mut self, u16 saturating/min/max, conditional clamp call	0,2,3,4,6
cursor-horizontal-margins	contract	Grid::{cursor_forward,cursor_backward,cursor_forward_margin,cursor_backward_margin}	∀ g∈Reach, n, m, inM=(L≤c≤Rm): cursor_forward(n): c'=min(c +sat n, MC(r)); cursor_backward(n): c'=c -sat n; cursor_forward_margin(n,m)=cursor_forward(n) if ¬m, else c'=min(c +sat n, inM ? min(Rm,MC(r)) : MC(r)); cursor_backward_margin(n,m)=cursor_backward(n) if ¬m, else c'=max(c -sat n, inM ? L : 0). All: r'=r, pw'=false, cells unchanged. (Production BS is cursor_backward(1); write_ops.rs::backspace_from_mid_column retargets to it.)	15	&mut self, u16 saturating/min/max, field reads	0,2,3,4,6
carriage-return	contract	Grid::{carriage_return,carriage_return_margin}	∀ g∈Reach, m: carriage_return(): c'=0; carriage_return_margin(m): c'=(m ∧ c≥L) ? L : 0. Both: r'=r, pw'=false, cells unchanged.	7	&mut self, early return, field writes	0,2,3,6
cursor-in-bounds	invariant	Grid::{set_cursor,cursor_up,cursor_down,line_position_relative,cursor_forward,cursor_backward,cursor_forward_margin,cursor_backward_margin,carriage_return,carriage_return_margin,line_feed,line_feed_margined,reverse_line_feed,reverse_line_feed_margined}	∀ g∈Reach with Inv(g), every listed motion and every argument: r'<R ∧ c'≤MC(r'). Corollary: on a 1×1 grid every motion leaves the cursor at (0,0). The functions' debug_assert!s are this clause and should become ensures.	1	&mut self, u16 arithmetic, type invariant over GridStorage	0,2,3,4,5,6
line-feed	contract	Grid::line_feed; Grid::scroll_region_up	∀ g∈Reach with region [t,b], cursor (r,c): r=b ⇒ scroll_region_up(1) and the cursor stays at (b,c); with [t,b] full screen and scrollback enabled the old top row becomes the newest history line. r<b, or r>b ∧ r<R-1 ⇒ (r+1,c), cells unchanged. r=R-1 below the region ⇒ cursor unchanged. In every arm pw'=false (also when it scrolls), display_offset'=0, and c' is clamped as in cursor-vertical-margins.	13	&mut self, match on Ordering, ring rotation and scrollback push in the callee	0,1,2,3,4,6,7,8
reverse-line-feed	contract	Grid::reverse_line_feed; Grid::scroll_region_down	∀ g∈Reach: r>t ⇒ r'=r-1, pw'=false, cells unchanged; r=t ⇒ r'=r, scroll_region_down(1) (rows t..b-1 move down one, row t := B) and pw'=pw (xterm RevScroll keeps it); r<t ⇒ r'=r -sat 1, pw'=false, cells unchanged. display_offset'=0; c' clamped as in cursor-vertical-margins.	11	&mut self, match on Ordering, row shifting callee	0,1,2,3,4,6,7,8
pending-wrap-motion-table	table-property	Grid::{insert_lines,delete_lines,insert_lines_margined,delete_lines_margined,scroll_region_up,scroll_region_down,scroll_up,line_feed,carriage_return,tab,back_tab,back_tab_margin}; GridCursorState::take_pending_wrap	∀ g∈Reach with pending_wrap=w. Keep w: scroll_region_up/down(n), scroll_up(n), tab(), back_tab(), back_tab_margin(..), set_scroll_region, and an IL/DL whose cursor row is outside the region or whose column is outside the DECSLRM margins. Clear: an IL/DL that passes both tests, line_feed() (moving or scrolling), carriage_return(). take_pending_wrap() returns the old flag and leaves false.	15	&mut self, per-op branches, struct copy of SavedCursor	0,2,3,5
write-char-denotation	refinement	Grid::{write_char,write_wide_char_styled,set_cell_complex_char,row_text,cell}; ComplexCharRing	For g∈Reach and ch, write_char(ch) at (r,c) without autowrap: narrow ch ⇒ cell(r,c).char()=ch (a BMP ch stored directly with COMPLEX clear; a supplementary ch sets COMPLEX and stores ch as complex text); a wide ch occupies (r,c),(r,c+1) as WIDE + WIDE_CONTINUATION; every other cell is unchanged, including rows that exist only because of earlier resizes (no ComplexCharRing aliasing); the cursor column becomes min(c+w,cols-1), the row is unchanged, the grid never scrolls; row len extends to c+w. set_cell_complex_char(r,c,s) makes (r,c) complex with text s without changing len. row_text(r) concatenates CellText per column up to len (char, complex string, combining marks), so writing s on a blank row from column 0 gives row_text(r)=s. (This is the screen model the read specs compute over.)	10	&mut self, char classification, Row cells via unsafe pages, CellExtras HashMap, ring index arithmetic, String	0,1,2,3,5,7,8,10
autowrap-deferred	contract	Grid::{write_char_wrap,write_char_wrap_styled,write_wide_char_wrap_styled,resolve_pending_wrap,advance_autowrap_line,wrap_serial}	∀ g∈Reach (full margins), ch: write_char_wrap(ch) (and the _styled and wide variants with their pen) first resolves a pending wrap, then places ch. A glyph ending on the last column leaves the cursor at cols-1 with pending_wrap:=true; otherwise the column advances by the width. Resolving (resolve_pending_wrap or the next glyph) is CR then advance_autowrap_line: row=bot ⇒ the region scrolls up 1 (full-screen pushes the top row into history); else row<R-1 ⇒ row+1; else (below the region at the bottom) no scroll. The landing row gets wrapped:=true, display_offset:=0, and wrap_serial increases by exactly 1. A wide glyph that does not fit wraps first. No other op (CR, LF, ordinary placement) changes wrap_serial; rows outside the region are never modified. (write_char_wrap is #[cfg(test)]; the spec is stated on write_char_wrap_styled and the wide variant, which it delegates to.)	8	&mut self, Row/Cell mutation, branches on region, ring scroll, u64 counter	0,1,2,3,4,5,8
run-writers-refine-write-char-wrap	equivalence	Grid::{write_ascii_blast,write_ascii_run_styled,write_cell_run,write_char_wrap,write_char_wrap_styled}	∀ g∈Reach with ¬cursor_outside_margin_span(), b∈[0x20..=0x7E]*, fg,bg,flags,last, ref=foldl(write_char_wrap_styled(_,fg,bg,flags), g, b): write_ascii_run_styled(b,fg,bg,flags,&mut last) returns |b|, sets last'=Some(b[|b|-1]) when b≠ε (else unchanged), and equals ref on View (every visible cell's char/colours/flags, wrapped flags, cursor, pending_wrap, history, wrap_serial, display_offset). write_ascii_blast(b) is the same with the default pen; write_cell_run(x,n,colors,flags,&mut last) returns n and equals n× write_char_wrap_styled(x). Corollaries: empty input changes nothing; filling a line exactly leaves cursor at cols-1 with pending_wrap; a wrap marks the next row wrapped; at the region bottom only the region scrolls. (write_ascii_run_styled and write_char_wrap are #[cfg(test)]; the production refinement is write_ascii_blast/write_cell_run vs write_char_wrap_styled.)	28	&mut self, byte slices, per-row chunked loops, PackedColors/CellFlags bit ops, Option<u8> out-param	0,1,2,3,4,5,7,8
writes-replace-extras	contract	Grid::{write_char,write_ascii_blast,write_cell_run,write_narrow_autowrap_fast,write_wide_autowrap_fast,write_wide_run_autowrap,write_mixed_wide_run_autowrap,write_ascii_run_with_extras,stale_extras_pair,fixup_wide_char_write,fixup_wide_chars_in_range,drop_unclaimed_extras}	For every production write path W landing cells on columns [a,b) of row r (both halves of each wide glyph included): afterwards each column in [a,b) holds exactly the extras W's pen specifies, starting fresh (no stale hyperlink, combining mark or orphaned map entry is adopted or merged), or none for a plain pen. Entries outside [a,b) are unchanged, except that wide halves the write orphans at either edge are blanked and their entries dropped; Row::len is tightened when that orphan was the row's last content (no phantom trailing space). A write that does not land changes nothing. ExtrasClaimed and WideCharConsistent hold afterwards.	11	&mut self over rows and the extras HashMap, slices of u8/char, Option<&Arc<str>> pens, wide-edge fixups, Row::len maintenance; one frame condition per path	0,1,2,3,4,5,7
wide-autowrap-tail-bce	contract	Grid::{write_wide_autowrap_fast,blank_wide_wrap_tail,fill_bce_rgb_range}; Row::clear_range_with_orphan	When write_wide_autowrap_fast(ch,colors,flags) runs with no pending wrap and cursor.col+1 ≥ ecols on row y: every cell of y in W=clear_range_with_orphan(cursor.col,ecols,B,B) (B=Cell::bce_blank(colors); W includes a bisected wide half left of cursor.col) becomes B, extras in W are dropped, and when colors.bg_is_rgb() the RGB ring entries for W hold the cursor template background; y gets EARLY_WRAP_HOLE iff ecols=cols ∧ cursor.col+1=cols; then the glyph is written as a wide pair at the start of the next line and the cursor advances per autowrap-deferred.	1	&mut self Grid, row_mut Option, Range<u16>, extras map, RGB ring, autowrap/scroll call	0,1,2,3,4,8
has-extras-iff-entry	invariant	Grid::{cell_extra_mut,cell_extra,set_cell_complex_char}; GridStorage::{cell_extra_mut,remove_cell_extra,sync_extras_flags_for_row}; CellExtras::set; Grid::{erase_to_end_of_screen,write_char}	∀ visible cell: HAS_EXTRAS(cell) ⇔ the map holds an entry with data at its coordinate (two-sided ExtrasClaimed). cell_extra_mut(r,c) creates an entry (default if absent), sets the bit, and whatever is written through it is what cell_extra(r,c) returns. remove_cell_extra(r,c) returns whether an entry existed, removes it, clears the bit, and leaves char and colours unchanged. CellExtras::set(k,e) with ¬has_data(e) removes the entry. sync_extras_flags_for_row(r,cols) re-establishes the equivalence on row r from any flag state. Erase ops clear the bit with the entry on every cell they blank and leave other rows alone; plain writes after a styled prompt produce no bits. set_cell_complex_char(r,c,s) sets COMPLEX and complex_char=s.	9	HashMap<CellCoord,CellExtra>, per-cell flag bits, &mut borrow returned from an entry, loops over a row	0,1,2,3,4,5,7,8
grid-cell-text-reads	contract	Grid::{resolved_char,row_text,row_text_into,row_cols_into,row_cols_prefix_into,visible_content}; VisibleRowView::{len,cell,is_wide_continuation,push_cell_text,col_char}	∀ g with Inv(g): resolved_char(r,c)=None when r≥rows or c≥cols; for a complex cell Some(complex_char_at(r,c)) or Some(U+FFFD) when absent; otherwise Some(cell.char()) (an unwritten cell reads ' '). row_text_into(r,out) clears out, returns false for r≥rows, else true with out = concat of CellText(V_r,c) over c<len(V_r) that are not wide continuations; row_text(r)=Some(that) iff row_text_into returns true. row_cols_prefix_into(r,p,out) clears out and for r<rows sets out=[col_char(V_r,c) | c<min(len(V_r),p)] ('\0' at a continuation, ' ' at a blank), returning 1+last c with out[c]≠' ', or 0. row_cols_into=row_cols_prefix_into(usize::MAX). visible_content() = concat over r<rows of row_text(r)++'\n'. Composed with write-char-denotation these give the asserted strings.	28	String and Vec<char> out-parameters, loops over a row view, extras lookups, &self	0,1,2,4,7
row-metadata-accessors	contract	Grid::{row_len,row_is_wrapped,visible_row_view}	∀ g∈Reach, viewport row v: row_len(v)=None and row_is_wrapped(v)=None iff v≥rows. Otherwise, whether the row is ring-resident or a history row reached through a scrolled-back viewport (where Grid::row(v)=None), row_len(v)=Some(1+index of the last non-blank cell, or 0) and row_is_wrapped(v)=Some(wrapped flag) (true exactly on soft-wrap continuations), both agreeing with row_text(v).	3	&self, Option, tier-aware view over ring rows vs tiered Line	0,1,2,3,12
buffer-access-lines	contract	impl BufferAccess for Grid: {char_at,line_len,line_text,line_wide_continuations,is_wide,is_line_wrapped,total_lines,display_offset}; buffer_access::{line_view,view_char}; Grid::{screen_row_view,materialize_scrollback_row_full}	∀ g, l:i32, view(g,l) = the LIVE screen row l when 0≤l<rows whatever display_offset; the history row with reverse index -l-1 (newest=-1) when -l-1<scrollback_lines; Empty otherwise. char_at(l,c)=view_char(view,c) (a complex cell yields its stored base char, e.g. a supplementary emoji, or U+FFFD); is_wide(l,c)=cell(view,c).is_wide(); line_len(l)=len(view) (0 for Empty); line_text(l)=None for Empty, row_text_screen for a live row, the concatenated view_chars for a history row; line_wide_continuations(l)=None for Empty else [is_wide_continuation(view,c) | c<cols]; is_line_wrapped(l)=view.is_wrapped(). Corollary: every read answers the same at every display offset 0..=scrollback_lines.	12	trait impl over a large &self structure, i32/i64 arithmetic, Arc, cross-crate scrollback materialization, String	0,1,2,4,7,10,12
viewport-memo-transparent	equivalence	Grid::{visible_row_view,materialized_history_row,history_absolute_row,materialize_scrollback_row_full}; ViewportRowCache::{lookup,store}	∀ g, v, d=display_offset(): v≥d ⇒ visible_row_view(v)=Live{screen_row=v-d, row=g.row(v)} (Empty if None), so d=0 always yields Live with screen_row=v; v<d ⇒ History{mat} with *mat=materialize_scrollback_row_full(d-1-v,cols()), or Empty when that is None. The memo is unobservable: the result equals the one with an empty cache across display scrolls, history growth while scrolled back (content follows the absolute row, not the index) and content changes.	3	&self with RefCell interior mutability, Arc<MaterializedRow>, enum with borrowed variants, u64 checked arithmetic, epoch struct equality	0,1,2,3,4,5,7
viewport-memo-sharing	contract	ViewportRowCache::{lookup,store,sync,index_of}; Grid::materialized_history_row; HistoryEpoch	store(e,a,r) then lookup(e,a), with no intervening store to index_of(a) and no sync to an epoch ≠ e, returns Some(r') with Arc::ptr_eq(r',r); lookup(e',a) after any epoch change (content_gen, history_renumber_epoch, cols or visible_rows) is None until a store under e'. At Grid level, for a keyable history row: two visible_row_view reads with no mutation between return pointer-equal Arcs; a display scroll that keeps the same absolute row on screen returns the same Arc at its new index; mark_content_full forces a fresh Arc with equal content.	3	RefCell borrow_mut, slot table Vec<Option<Slot>>, Arc pointer identity, u64 modular indexing	0,1,2,3,5
scroll-region-shift	contract	Grid::{scroll_region_up,scroll_region_down,scroll_region_up_margined,scroll_region_down_margined,scroll_up,scroll_down}; CellExtras::shift_region_*	∀ g∈Reach (and the defensive top=bottom state outside Reach), n, region [t,b] with t<b, k=min(n,b-t+1), Scr' the rows after: scroll_region_up(n): Scr'[i]=Scr[i+k] for t≤i≤b-k, blank (template B) for b-k<i≤b, Scr[i] outside [t,b]; scroll_region_down(n): Scr'[i]=Scr[i-k] for t+k≤i≤b, blank for t≤i<t+k. Extras move with their rows. The margined variants restrict move and blanking to columns [L,Rm]. scroll_up/scroll_down are the full-screen cases (scroll_down is #[cfg(test)]; retarget to scroll_region_down on the full region). With scrollback enabled scroll_up appends the k removed rows to history oldest first and evicts beyond the cap, across both the ring-growth and ring-reuse phases of one call. n=0 and t=b are the identity. Every variant sets display_offset=0, keeps cursor and pending_wrap, and leaves |ring_extras|=ring_buffer_scrollback. (Top-anchored archival: top-anchored-region-archive.)	34	&mut self, ring rotation over Vec<Row>, row recycling into PageStore (unsafe), CellExtras offset shift, history push to tiered store	0,1,2,3,4,5,7,8,12
scroll-up-appends-transcript	refinement	Grid::{scroll_up,scroll_up_storage,grow_scrollback_ring,reuse_scrolled_rows,line_feed,drain_lazy_buffer}; GridStorage::linearize_ring; LazyBuffer staging	∀ g∈Reach, n≥1, scroll_up(n) (and a line_feed at the bottom of a full-screen region, n=1) refines Transcript'=Transcript ++ blank^n, with Rows' = the last rows elements and Hist' = the newest cap elements of the rest (cap=max_scrollback ring-only; ring cap + lazy + store limit tiered). Each archived line is L(R_i) of the row that scrolled off (text, attrs, wrapped flag, extras), in age order across the tiers. scrollback_lines() = ring+lazy+store ≤ cap, total_lines ≤ rows+max_scrollback, the bottom rows are blank. In particular every line feed after a rows-only grow of a wrapped ring (ring_head≠0, below the raised cap) scrolls every row.	6	ring buffer with modular head, Vec rotation/linearize, DeferredLine staging, cross-crate ScrollbackStorage push, budget enforcement, loops	0,1,2,3,4,5,7,8,12
top-anchored-region-archive	contract	Grid::{scroll_region_up,scroll_top_anchored_region_up_with_history}; GridStorage::presentation (absolute/selection row updates, damage); CellExtras dense ring shift	Region [0,b] (b<rows), n≥1, history capacity present: scroll_region_up(n) with n'=min(n,b+1) appends rows[0..n'] to history in order with their extras, dense RGB and complex chars, except (i) while the grid holds no history the leading prefix of never-written rows is dropped (a written-then-erased row and an interior blank after history exists do archive) and (ii) with no history capacity (ring 0 and no store) nothing is archived (a zero-ring tiered store still archives). Rows[0..=b] become rows[n'..=b] ++ blank^{n'} with the BCE template (truecolour bg included). Footer rows b+1.. are unchanged, extras included, and not damaged; damage is exactly rows 0..=b; content_gen +1; take_content_scroll_delta stays 0. When a>0 rows archive, take_absolute_row_update and take_selection_row_update each return Splice{at: old_live_top+b+1, inserted: a}; successive scrolls coalesce by summing; no archive gives None. At ring capacity history stays ordered and capped; full-screen with n>rows archives exactly one screenful; the same holds after a rows-only grow of a wrapped ring.	15	&mut self, ring + footer shift, Option<AbsoluteRowUpdate> coalescing, dense rings, damage bitmaps, u64 counters, loops	0,1,2,3,4,5,7,8,12
top-anchored-scroll-refinement	refinement	Grid::{set_scroll_region,scroll_region_up,scroll_region_up_margined,take_absolute_row_update,take_selection_row_update,take_content_scroll_delta,take_selection_damage}; SelectionDamage::clears_selection; TextSelection::{adjust_for_row_splice,adjust_for_scroll,intersects_absolute_band,truncate_to_floor}	∀ g (scrollback on or off), region [top,bottom], full-width or margined scroll by 1, selection disjoint from or overlapping the region: the observed (history_len delta, footer preserved, footer_anchor splice size, selection_alive, selection rows) after the scroll plus Terminal::post_process's selection pipeline is a Scroll successor of top_anchored_scroll_history_model at Buggy=0. History grows by 1 (Splice{inserted:1}) iff top=0 ∧ full width ∧ scrollback enabled, else 0 with no splice; rows below bottom unchanged; an overlapping selection is remapped by the splice and stays alive; a disjoint selection survives unmoved in all four regimes. The 'scroll to screen bottom' successor (Buggy=1) is the negative control.	1	&mut self, enum results, u64 absolute counters, i32 conversions, cross-crate selection type and closures, model projection	0,1,2,4,10,12,13
el-span	contract	Grid::{erase_to_end_of_line,erase_from_start_of_line,erase_line,erase_to_end_of_line_core,erase_from_start_of_line_core,erase_line_impl}; Row::{clear_range_with_orphan,erase_with}	∀ g∈Reach, w=EC(r): EL0 sets cells [c,w) of row r to B; EL1 sets [0,min(c+1,w)); EL2 sets every cell of row r and keeps LS(r). No DECSLRM margin narrows the span. Cells of row r outside the span are unchanged except the wide half the span orphans (head at c-1 for EL0, continuation at the span end for EL1), which is blanked; a wide glyph parked on its continuation loses both halves. Every other row is unchanged; extras inside the span and of a blanked orphan are dropped (HAS_EXTRAS cleared); the cursor does not move.	18	&mut self, Row cell slices, HashMap extras, wide-pair fixup, loops	0,1,2,3,4,5,6,7
erase-bce-orphan	contract	Grid::{erase_to_end_of_line_core,erase_from_start_of_line_core,erase_line_impl,fill_bce_rgb_range}; Row::clear_range_with_orphan	∀ g∈Reach: for EL0/EL1/EL2 (and the ED cursor row) the BCE span [s,e) returned by clear_range_with_orphan covers the erased span plus any orphaned wide half; every cell in it takes B's colours (an orphan is not EMPTY), its extras are dropped, and when B_rgb=Some(x), bg_rgb_at(r,j)=Some(x) for every j∈[s,e). (The rect ops instead give their orphan Cell::EMPTY: rect-fill-erase.)	5	&mut self, Range return value, ring writes	0,1,2,3,4,6,7
ed-span	contract	Grid::{erase_to_end_of_screen,erase_from_start_of_screen,erase_screen,erase_below_cursor,erase_above_cursor,clear_rows}	∀ g∈Reach: ED0 = EL0 on row r, then every row in (r,R) := B; ED1 = every row in [0,r) := B, then EL1 on row r; ED2 = every visible row := B, extras cleared, display_offset'=0. Margins never narrow the cursor-row span. Rows outside the span keep their cells; the cursor does not move. Holds on 1-row and 1×1 grids. Line attributes: ed-line-attributes.	14	&mut self, Range loops over rows, HashMap clear	0,1,2,3,4,6,7
ed-line-attributes	contract	Grid::{erase_screen,erase_to_end_of_screen,erase_from_start_of_screen,erase_line,selective_erase_screen,clear_rows,resync_any_double_width}	∀ g∈Reach: ED2 ⇒ ∀i LS'(i)=SingleWidth and any_double_width'=false (so MC(i)=C-1 again); ED0 ⇒ LS'(i)=SingleWidth for i∈(r,R), LS'(i)=LS(i) for i≤r; ED1 ⇒ LS'(i)=SingleWidth for i<r, LS'(i)=LS(i) for i≥r; EL2 and every DECSEL/DECSED keep every LS. After ED0/ED1, any_double_width' = ∃i LS'(i)≠SingleWidth.	7	&mut self, any() rescan loop, enum field	0,1,2,3,4,5,6,7
erase-pending-wrap-table	table-property	Grid::{erase_to_end_of_line,erase_from_start_of_line,erase_line,erase_to_end_of_screen,erase_from_start_of_screen,erase_screen,screen_alignment_pattern,erase_scrollback,erase_rect,fill_rect,selective_erase_rect,change_attrs_rect,change_attrs_stream,reverse_attrs_rect,reverse_attrs_stream,copy_rect}	∀ g∈Reach and arguments: pw'=false after EL0/1/2, ED0/1/2 and DECALN (including ED0 at the origin of a 1-column grid and ED1 at bottom-right); pw'=pw after ED3, DECERA, DECFRA, DECSERA, DECCARA, DECRARA (rect and stream) and DECCRA. Composed with autowrap-deferred: after a reset with the cursor parked on the last column, the next printable overwrites (r,C-1), re-arms pw and moves no row.	15	&mut self, one bool field per op; composition with the write path	0,2,3,4,6
right-erase-breaks-wrap-link	contract	Grid::{clear_wrap_into_next_row,erase_to_end_of_line_core,erase_chars,erase_from_start_of_line,erase_line}	∀ g∈Reach, r<R-1: after EL0, DECSEL0, ED0/DECSED0 (cursor row) at any column, or ECH(n) with c+min(n,EC(r)-c)=EC(r): Wr'(r+1)=false and row r+1's cells unchanged. After EL1, EL2, or an ECH ending short of EC(r) (DECSLRM margins do not move EC(r)): Wr'(r+1)=Wr(r+1). No erase on row r changes Wr(r). On r=R-1 no link changes and nothing panics.	6	&mut self, Option<&mut Row> let-chains, flag bit	0,2,3,4,6
selective-erase-span	contract	Grid::{selective_erase_to_end_of_line,selective_erase_from_start_of_line,selective_erase_line,selective_erase_to_end_of_screen,selective_erase_from_start_of_screen,selective_erase_screen}; Row::{selective_clear_range,selective_clear}	Spans: DECSEL0 {r}×[c,EC(r)); DECSEL1 {r}×[0,min(c+1,EC(r))); DECSEL2 {r}×[0,C); DECSED0 = DECSEL0 span ∪ (r,R)×[0,C); DECSED1 = [0,r)×[0,C) ∪ DECSEL1 span; DECSED2 = [0,R)×[0,C). Margins never narrow a span. ∀ g∈Reach, (i,j) in the span: Prot(i,j) ⇒ Cell'(i,j)=Cell(i,j) with its extras; ¬Prot(i,j) ⇒ Ch'(i,j)=' '. Cells outside the span unchanged. With no protected cell the characters equal those of the matching EL/ED.	11	&mut self, per-cell loops, flag tests	0,1,2,3,4,5,6,7
selective-erase-pending-wrap	contract	Grid::{selective_erase_to_end_of_line,selective_erase_from_start_of_line,selective_erase_line,selective_erase_to_end_of_screen,selective_erase_from_start_of_screen,selective_erase_screen,reset_wrap_unless_span_protected}	U(S) = ∃(i,j)∈S ¬Prot(i,j) (an unmaterialized row counts as unprotected where its part of S is non-empty). ∀ g∈Reach: DECSEL0: pw'=false; DECSEL1: pw'=pw ∧ ¬U({r}×[0,c+1)); DECSEL2: pw'=pw ∧ ¬U({r}×[0,C)); DECSED0: (r,c)=(0,0) ? pw'=pw ∧ ¬U([0,R)×[0,C)) : pw'=false; DECSED1: pw'=pw ∧ ¬U([0,r)×[0,C) ∪ {r}×[0,c+1)); DECSED2: pw'=pw ∧ ¬U([0,R)×[0,C)). A selective clear never changes protection, so U may be read before or after it.	16	&mut self, closure capturing &Self, any() over ranges, Option	0,2,3,4,5,6,7,13
ed3-erase-scrollback	contract	Grid::erase_scrollback	∀ g∈Reach (including a full ring with ring_head≠0 and a tiered store): after erase_scrollback: scrollback_lines'=0, tiered_scrollback_lines'=0 (ring, lazy and store empty), total_lines'=R, ring_head'=0, display_offset'=0, Rows'=Rows cell for cell, scrollback_clear_gen'=gen wrapping+1, cursor and pw unchanged, Inv holds.	4	&mut self, Vec rebuild with unsafe Row::new, ring linearization, tiered store clear with a logged error	0,1,2,3,4,6,7,8,12
decaln	contract	Grid::screen_alignment_pattern	∀ g∈Reach: after DECALN, ∀ i<R, j<C: Cell'(i,j)='E' with empty flags and default colours; LS'(i)=SingleWidth; any_double_width'=false; extras cleared; cursor'=(0,0); scroll region full(R); horizontal margins reset; pw'=false; history unchanged.	8	&mut self, nested loops over rows and cols	0,1,2,3,5,6,7,8
rect-fill-erase	contract	Grid::{fill_rect,erase_rect,drop_unclaimed_extras}; Row::clear_range_with; CellExtras::set_rgb_ring_range	∀ g∈Reach, f,t,l,b,rt,fg,bg: t^=min(t,R-1), b^=min(b,R-1), l^=min(l,C-1), r^=min(rt,C-1). t^>b^ or l^>r^ ⇒ G'=G. Else ∀ i∈[t^,b^], j∈[l^,r^]: Cell'(i,j)=f (char, flags, colours) and the ring resolves fg/bg to the given bytes where Some; a wide pair cut by the rect edge has its outside half set to Cell::EMPTY (never f, never B); a protected narrow cell beside the edge is not read as a continuation and is unchanged; every other cell unchanged; cursor and pw unchanged. erase_rect(t,l,b,r) = fill_rect(B,t,l,b,r,None,B_rgb).	14	&mut self, nested loops, ring writes, HashMap clear_rect	0,1,2,3,4,5,6,7
decsera	contract	Grid::{selective_erase_rect,remove_cell_extra}; Row::selective_wipe_range; CellExtras::scrub_char_marks	Same clamp and empty guard as rect-fill-erase. ∀ cell in the rect: Prot ⇒ unchanged; ¬Prot ⇒ char:=' ', SGR flags and colours kept, combining marks and complex text removed from its extra, and the extra is dropped (with HAS_EXTRAS) iff no colour-bearing field remains. A wide pair the rect touches is wiped on both halves with no WIDE/WIDE_CONTINUATION left. Cursor and pw unchanged.	4	&mut self, Vec<u16> scratch, HashMap extras, nested loops	0,1,2,3,4,5,6,7
deccara-decrara	contract	Grid::{change_attrs_rect,change_attrs_stream,reverse_attrs_rect,reverse_attrs_stream,edit_attrs}; Row::recompute_len	∀ g∈Reach, with the rect clamp, every cell of the extent gets flags'=(flags∖clr)∪set (DECCARA) or reverse_cell_attrs(flags,toggle,u) (DECRARA). Rect extent=[t^,b^]×[l^,r^] (empty if l^>r^); stream extent = row t^ from l^, middle rows whole, row b^ through r^. Cells outside the extent unchanged. Every touched row ends with Len = 1+last non-empty column (0 if none), so flags set on blank cells grow Len and clearing them shrinks it. Cursor and pw unchanged.	4	&mut self, impl Fn closure parameter, nested loops, get_mut	0,1,2,3,4,5,6,7,13
reverse-cell-attrs	contract	Grid::reverse_cell_attrs	∀ flags f, toggle t, u:bool, x=f⊕t, UL=UNDERLINE|DOUBLE_UNDERLINE|CURLY_UNDERLINE: result = ¬u ? x : (x∩UL≠∅ ? x∖UL : x∪UNDERLINE). Corollaries: with u every underline style (single, double, curly, dotted, dashed) maps to none and none to single; bits outside UL∪t are unchanged.	1	pure fn over a bitflags newtype (xor/and/or), const	0,1,3,6
deccra-copy	contract	Grid::copy_rect; Row::{is_cell_wide_continuation,fixup_wide_chars_in_range,recompute_len}	∀ g∈Reach: clamp the source rect; empty ⇒ G'=G. buf = snapshot of the source cells before any write (overlap-safe), with its first cell := EMPTY iff it is a genuine wide continuation (a protected narrow cell is copied verbatim) and its last cell := EMPTY iff WIDE. For each buf cell whose destination (min(dt,R-1)+di, min(dl,C-1)+dj) is in the grid: Cell'=buf[di][dj] with the source cell's extras, ring truecolour and ring complex codepoint; stale destination extras/rings are cleared first; wide pairs cut by the destination edges are blanked; each written row's Len = content length. Source-only cells, cursor and pw unchanged.	7	&mut self, Vec<Vec<Cell>> buffers, HashMap clone, zip iterators	0,1,2,3,4,5,6,7
ich-dch-shift	contract	Grid::{insert_chars,delete_chars,insert_chars_margin,delete_chars_margin,shift_is_bounded}; Row::{insert_chars_fill,insert_chars_bounded_fill,delete_chars_fill,delete_chars_bounded_fill}; CellExtras::{shift_cols_right,shift_cols_left}	∀ g∈Reach, n, m, w=EC(r): bound=min(Rm+1,w) for insert_chars/delete_chars; for the _margin variants with m, bound=(L≤c≤Rm) ? min(Rm+1,w) : w (m=false ⇒ the plain variant). Always pw'=false. If c<bound, with k=min(n,bound-c): ICH: cells [c,c+k):=B, [c+k,bound):=old [c,bound-k), old [bound-k,bound) dropped; DCH: [c,bound-k):=old [c+k,bound), [bound-k,bound):=B. Row r outside [c,bound) and all other rows unchanged; extras move with their cells, blanks carry none and deleted cells' extras are dropped; a wide pair the shift cuts is blanked. c≥bound ⇒ cells unchanged. n=0 is the identity. Cursor unchanged. Corollary: DCH(k) after ICH(k) at the same cursor restores [c,bound-k).	30	&mut self, slice copy_within, HashMap key shifting	0,1,2,3,4,5,6,7
ech-erase-chars	contract	Grid::erase_chars; Row::erase_chars_with	∀ g∈Reach, n: pw'=false; if c<EC(r), with k=min(n,EC(r)-c): cells [c,c+k):=B and their extras dropped, no cell shifts, the rest of row r unchanged except a wide half orphaned by the span (blanked), other rows unchanged; DECSLRM margins never bound the span. The cursor does not move. (The wrap-link rule is right-erase-breaks-wrap-link.)	13	&mut self, range clear, extras clear_range	0,1,2,3,4,6,7
il-dl	contract	Grid::{insert_lines,delete_lines}; GridStorage::{shift_visible_rows_down,shift_visible_rows_up}; CellExtras::shift_region_*	∀ g∈Reach, n:usize: n=0 or r∉[top,bot] ⇒ G'=G (cursor and pw included). Else with k=min(n,bot-r+1): IL: rows [r+k,bot]:=old [r,bot-k], rows [r,r+k):=single-width B rows; DL: rows [r,bot-k]:=old [r+k,bot], rows (bot-k,bot]:=single-width B rows. Extras move with their rows; rows outside [r,bot] unchanged; c'=L; r'=r; pw'=false; display_offset'=0; total_lines unchanged. Corollary: DL(k) after IL(k) at r restores rows [r,bot-k].	35	&mut self, row ring rotation, usize/u16 conversions, HashMap key shifts	0,1,2,3,4,5,6,7
il-dl-margined	contract	Grid::{insert_lines_margined,delete_lines_margined}; Row::{overwrite_span,fixup_wide_boundary,recompute_len}	∀ g∈Reach, n, m: ¬m or margins full ⇒ same as insert_lines/delete_lines. Else n=0, r∉[top,bot], or c∉[L,Rm] ⇒ G'=G (cursor column included). Else the il-dl shift applies to the rectangle rows [r,bot] × columns [L,Rm] only (vacated cells := B, other columns unchanged, wide pairs cut at L/Rm blanked), k=min(n,bot-r+1), c'=L, pw'=false, and every rewritten row's Len = 1+last non-empty column.	10	&mut self, Vec<Cell> span buffer, reverse loops, slice copy_within	0,1,2,3,4,5,6,7
decic-decdc	contract	Grid::{insert_columns,delete_columns,scroll_left,scroll_right,back_index,forward_index}	∀ g∈Reach, n: pw'=false. insert_columns/delete_columns: n=0, c∉[L,Rm], or r∉[top,bot] ⇒ cells unchanged; else ∀ i∈[top,bot] row i gets the bounded ICH/DCH of ich-dch-shift at column c with bound=min(Rm+1,C), k=min(n,bound-c); columns outside [L,Rm] and rows outside the region unchanged. (scroll_left/right and back/forward_index are the same shift at column L; no samples yet.)	5	&mut self, loop over region rows	0,1,2,3,4,6,7
unscroll-denotation	contract	Grid::{unscroll_from_scrollback,fill_row_from_line}; GridStorage history pop (ring, lazy, store)	Region [t,b], n: n=0 is a no-op returning 0. With no scrollback attached and empty history it falls back to scroll_region_down(n) and returns 0 (the result counts history lines recovered, not rows moved). Otherwise k=min(n,scrollback_lines(),b-t+1) over ring+lazy+store. If all k newest history lines read OK: rows[t..=b] = [fill_row_from_line(h) | h in the newest k lines, oldest first] ++ rows[t..=b-k]; those k lines leave history (no duplicates; age order holds across the ring/store seam); rows outside [t,b] unchanged; damage is exactly rows t..=b; returns k. If any of the k reads fails (I/O or decompression), it returns 0 and neither the grid nor the history changes.	14	Result-typed history reads across tiers (disk tier is fs I/O), all-or-nothing update, Vec<Line>, loops	0,1,2,3,4,5,7,8,12
row-band-move-compose	contract	Presentation::{record_row_band_move,take_row_band_moves,poison_row_band_moves}	record(top,bottom,δ) is a no-op when δ=0 or top>bottom. Otherwise with N the newest entry: if N.bottom=bottom, sign(N.δ)=sign(δ), and (N.top=top or (N.δ>0 ∧ top=N.top+N.δ)), then N.δ:=N.δ+δ (inexact:=true if that overflows i16). Otherwise the move is appended when len<ROW_BAND_MOVES_PER_BATCH (8), and inexact:=true when full; a move is never dropped silently. take returns the record and resets it. The record denotes the composition of its moves in order exactly when inexact=false. Finite state.	1	[RowBandMove;8] with u8 len, Option get_mut, i16 checked_add/signum, mem::take	0,1,2,3
row-band-moves-denote-ops	contract	Grid::{scroll_region_down,scroll_region_up,line_feed,insert_lines,delete_lines,scroll_region_up_margined,insert_columns,force_selection_invalidation,invalidate_host_coordinates,take_coordinates_invalidated}	Every full-width row-translate op records exactly the band it applies and sets coordinates_invalidated: scroll_region_down(n) on [t,b] records (t,b,+n); scroll_region_up(n) records (t,b,-n) (a top-anchored archival scroll also yields its Splice, and history grows by the archived count); IL(n) at row c records (c,b,+n), DL(n) (c,b,-n). A line_feed that does not scroll records, splices and archives nothing. Margined SU, DECIC, force_selection_invalidation and invalidate_host_coordinates set inexact and the flag. The record drains with take_row_band_moves.	4	&mut self, callee-ensures composition of record_row_band_move, bool flags, enum Splice	0,1,2,3,4
viewport-scroll-damage	contract	Grid::{scroll_display,scroll_to_top,scroll_to_bottom,clamp_display_offset}; Damage::{mark_rows,mark_full}	∀ g∈Reach with H=scrollback_lines(), R rows, offset o: scroll_display(d) sets o'=clamp(o+d,0,H); scroll_to_top o'=H; scroll_to_bottom o'=0; clamp_display_offset o'=min(o,H). With D=|o'-o| the damage added is: none if D=0; exactly rows [0,D), not full, when o'>o ∧ D<R; exactly rows [R-D,R), not full, when o'<o ∧ D<R (for clamp_display_offset, the bottom D rows); full when D≥R. Content and content_gen unchanged.	11	&mut self, i32/usize saturating arithmetic, Damage bitset	0,1,2,3,5,7
display-offset-navigation	contract	Grid::{scroll_display,scroll_to_bottom,scroll_to_top,top_visible_absolute_row,scroll_to_absolute_row}	top_visible_absolute_row() = visible_to_absolute(0) - display_offset. scroll_to_absolute_row(a) sets display_offset := clamp(visible_to_absolute(0)-a, 0, scrollback_lines()): a target in [oldest, live_top] lands exactly at the top, one at or below the live top gives 0, one older than oldest_absolute_row() gives scrollback_lines(). DisplayOffsetValid holds throughout. (The offset arithmetic of scroll_display/scroll_to_* is in viewport-scroll-damage.)	3	i32/u64/usize mixed arithmetic with saturating clamps, &mut self	0,2,3,4,6
content-damage-and-generation	contract	Grid::{clear_damage,needs_full_redraw,scroll_up,scroll_region_up,erase_to_end_of_screen,take_content_scroll_delta}; GridStorage::mark_content_*	∀ g∈Reach: needs_full_redraw() ⇔ damage().is_full(); clear_damage() empties the damage. scroll_up(n) with n<R marks exactly the bottom n rows and with n≥R marks full; scroll_region_up on [t,b] marks only rows in [t,b]; ED0 from row r marks only rows ≥ r. Every mark_content_* call adds its damage and increments content_gen by 1. content_scroll_delta accumulates the n of every content scroll_up since the last take; take_content_scroll_delta() returns it and resets it to 0.	4	&mut self, Damage row bitset, u64/i32 counters	0,2,3,5
mark-cursor-damage	contract	Grid::mark_cursor_damage; Damage::{mark_cell,iter_bounds}	From an undamaged state, mark_cursor_damage() gives partial (not full) damage whose iter_bounds yields exactly one bound, with line=cursor.row and left ≤ cursor.col < right.	2	&mut self, damage bitmap and bounds iterator	0,1,2,3,7
content-gen-tracks-content	invariant	Grid::content_gen; constructors; every content mutator (write_char, erase_line, scroll_up, scroll_region_up, line_feed, set_scrollback_line_limit); viewport ops (scroll_display, scroll_to_top, scroll_to_bottom)	content_gen>0 for every constructed grid and never decreases. A viewport-only op (scroll_display(any d), scroll_to_top, scroll_to_bottom), scroll_up(0) and a retention re-apply that evicts nothing leave it unchanged. For every op, if the live row text (at display_offset 0) or the retained history changes, content_gen strictly increases; a single region scroll increases it by exactly 1, and a retention shrink that evicts lines (ring-only or tiered) increases it.	6	u64 counter as a two-state relation over every mutator; needs the content observation as a model	0,1,2,3,4,5,7
absolute-row-numbering	invariant	Grid::{visible_to_absolute,oldest_absolute_row,base_y,display_origin_absolute,history_renumber_epoch,with_tiered_scrollback,adjust_row_count_rows_only,keep_keys_across_bottom_rows,cover_retained_rows}	∀ g∈Reach: KeysCoverRetained holds and the read map equals the write map: visible_to_absolute(v)=absolute_row_counter-rows+v = oldest_absolute_row()+scrollback_lines()+v. base_y()=oldest_absolute_row()+scrollback_lines(); display_origin_absolute()=base_y()-display_offset. A content scroll by k (ring-growth or ring-reuse phase) adds k to the counter, so visible_to_absolute'(v-k)=visible_to_absolute(v). with_tiered_scrollback over a store of k lines gives scrollback_lines()=k, oldest_absolute_row()=0, visible_to_absolute(0)=k, counter=k+rows. A rows-only resize (reveal, blank append, blank trim, demotion) keeps every retained row's key and history_renumber_epoch, and moves oldest_absolute_row only by the rows evicted past the cap, also on a grid that grows before holding any history.	6	u64 counters with saturating subtraction, usize→u64 casts, a two-state relation over keys	0,2,3,4,5,6
continue-absolute-numbering	contract	Grid::{continue_absolute_numbering,visible_to_absolute}	∀ g∈Reach, x:u64: counter'=max(min(x, u64::MAX>>16), R+scrollback_lines()); visible_to_absolute(i)=counter-R+i.	1	&mut self, u64 min/max, call	0,2,3,4,6
history-line-order	contract	Grid::{get_history_line,try_get_history_line,history_line_rev,try_history_line_rev,history_line_count}	∀ g∈Reach (ring-only or tiered, any mix of store, lazy buffer and ring rows), H=h_0..h_{N-1} the rows that scrolled off, oldest first, as retained under the caps: N=history_line_count()=scrollback_lines(); get_history_line(i)=Some(h_i) iff i<N; history_line_rev(j)=get_history_line(N-1-j), None for j≥N; try_get_history_line and try_history_line_rev return Ok of the same Option whenever no tier fails, which is always on the ring-only path.	3	&self, Cow<Line>, index arithmetic across three tiers, Result	0,1,2,3,4,12
history-read-errors	contract	Grid::{try_get_history_line,get_history_line,try_history_line_rev}; SearchContent::get_row_text for Grid	∀ g whose attached store returns Err for line i: try_get_history_line(i)=Err(_), get_history_line(i)=None, try_history_line_rev(N-1-i)=Err(_), SearchContent::get_row_text on that row = None, and none of them panics.	4	Result propagation from a disk-backed store (fs, mmap), &self	0,1,2,8,12
history-walk-refines-get	equivalence	Grid::{history_lines_from,get_history_line}	∀ s: history_lines_from(s) has len=scrollback_lines().saturating_sub(s) and yields item for item get_history_line(s+k) for k<len, across the store (cold/warm/hot), lazy buffer and ring. An unreadable line yields None at its own index and every later line keeps its index.	2	iterator struct over three sources (closures/adaptors), Option<Arc<Line>>, cross-crate store with corrupt blocks	0,1,3,4,5,7,12,13
history-fence-export	contract	Grid::{history_fence,history_lines_since_fence}; HistoryFence::{lines,holds_at}	∀ g∈Reach, F=history_fence(g), chunk k≥1: while F.holds_at(history_fence(g_now)), successive history_lines_since_fence(F,from,k) over [F.oldest,F.end) return Ok, non-empty below F.end, and concatenate to H(g) at fence time oldest first (F.lines()=scrollback_lines()). Plain output after the fence keeps holds_at true, puts new lines at keys ≥ F.end and never inside the read.	2	&self over tiered scrollback (compressed blocks), Vec<Line>, Result	0,1,3,4,5,6,7,12
history-fence-breaks	contract	HistoryFence::holds_at; Grid::{history_lines_since_fence,resize,erase_scrollback}	holds_at(F,N) ⇔ ¬F.detached ∧ ¬N.detached ∧ F.renumber_epoch=N.renumber_epoch ∧ F.clear_gen=N.clear_gen ∧ F.reveal_gen=N.reveal_gen ∧ F.cols=N.cols ∧ N.end≥F.end. history_lines_since_fence(F,from,k) = Err(Moved) if ¬holds_at(F,now); Err(Evicted) if it holds and from<now.oldest; else Ok. A width reflow (epoch+1), ED3 (clear_gen+1) and a rows-grow that reveals history (reveal_gen+1) break every earlier fence; a rows shrink and plain output do not; retention eviction keeps the fence but makes reads below the new oldest Err(Evicted).	4	pure struct comparison, plus &mut self resize/erase paths	0,1,2,3,4,6,12
reserve-older-history-keys	contract	Grid::{reserve_older_history_keys,older_history_claim_holds}	∀ g∈Reach, n:u64: counter'=counter +sat (n -sat oldest_absolute_row(g)); when oldest ≥ n no visible or retained key moves. The claim records scrollback_clear_gen; older_history_claim_holds(claim) ⇔ scrollback_clear_gen unchanged since.	1	&mut self, u64 saturating arithmetic	0,1,2,3,6
attach-older-history	contract	Grid::attach_older_history; OlderHistory::build	∀ g∈Reach, L, claim: refusals in order, each returning the unconsumed OlderHistory with g unchanged: claim no longer holds ⇒ Cleared; store detached ⇒ ReflowInFlight; older.cols≠C ⇒ WidthMoved{now:C}; no in-memory tiered store ⇒ NoTieredStore. Otherwise H(g') = the suffix of lines(older)++H(g) kept by g's own line limit and memory budget (oldest imported first evicted; H(g) stays newest; the imported remainder is contiguous); the result = imported lines retained; base_y and visible keys unchanged; counter' ≥ R+scrollback_lines'; history_renumber_epoch strictly increases; the line limit is unchanged.	6	&mut self, Box<Scrollback> moves, enum results carrying owned data, store push loops	0,1,2,3,4,6,7,12
older-history-build	contract	OlderHistory::{build,lines,cols}; scrollback_reflow::reflow_scrollback_lines	∀ L, from, cols: build(L,from,cols).cols()=cols; lines()=|L| if from=cols or L is empty, else |reflow_scrollback_lines(L,cols)| (a line of n narrow cells becomes ceil(n/cols) rows).	1	Vec<Line>, String, store construction, loops	0,1,3,5,6,7,12
retention-limit	contract	Grid::{set_scrollback_line_limit,scrollback_line_limit}; GridStorage::{attach_scrollback,set_store_line_limit,scrollback_line_limit,scrollback}; ScrollbackStorage::line_limit	Ring-only: set_scrollback_line_limit(Some(L)) makes L the ring cap; a shrink evicts oldest-first at once to L (newest lines and the viewport untouched, oldest_absolute_row += evicted, display_offset re-clamped ≤ L); a raise allocates no ring rows up front and retention grows lazily to L in write order (even from a wrapped ring); None is unlimited; the getter returns the setter's value (initially Some(max_scrollback)). Tiered with ring cap R: Some(L) limits ring+lazy+store to L; L≥R ⇒ the ring keeps R and the store share is L-R; L<R ⇒ the ring is re-capped to L and the store share is 0, and a later raise widens only the store share; L=R discards ring evictions directly with no staged or stored lines; None is an unlimited store; the getter returns the total (initially DEFAULT_LINE_LIMIT+R). GridStorage: after attach_scrollback(s), scrollback().is_some(); after set_store_line_limit(Some(k)), scrollback_line_limit()=Some(k+max_scrollback). At steady state scrollback_lines()=min(L, lines scrolled off), newest kept.	11	Option<usize> arithmetic, ring eviction loops, cross-crate store limit, &mut self	0,1,2,3,4,5,7,12
unified-retention-limit-refinement	refinement	Grid::{with_tiered_scrollback,set_scrollback_line_limit,scrollback_line_limit,line_feed,scrollback_mut,ring_buffer_scrollback,scrollback_lines}; ScrollbackStorage::{set_line_limit,line_count}; retention_limit_spec.rs	∀ tiered grid with ring cap R and limit L (both the L≥R and the L<R arm): after set_scrollback_line_limit(Some(L)), scrollback_line_limit()=Some(L), and the quiescent projection (ring_buffer_scrollback, store.line_count) refines UnifiedRetentionLimit (Buggy=0) under each scroll-off; hence ring ≤ R, ring+store ≤ L and scrollback_lines() ≤ L at every quiescent point. The model's own invariants are checked by ty check (Buggy=1 counterexample), which moves to the verify model lane. The raw-store bypass (capping the store at L directly) is this spec's negative control and must violate TotalBounded.	3	Grid &mut self, scrollback store tiers, Option<usize>, loops, model-to-code projection	0,1,2,7,10
ring-watermark-level	contract	Grid::{ring_watermark_level,set_ring_byte_watermark,memory_used}	With no budget configured the level is Green. With budget B and u=memory_used(): Green when u<0.80·B, Yellow when 0.80·B ≤ u < 0.95·B, Red when u ≥ 0.95·B.	1	Option<usize>, integer ratio comparison, enum result	0,1,3,4
grid-memory-accounting	contract	Grid::{memory_used,drop_resize_undo}; CellExtras::memory_used; CellExtra::memory_used; LazyBuffer::memory_used	memory_used(g) is the saturating sum in content_queries.rs: pages, extras, row and tab-stop capacity, ring extras and their retained bytes, resize undo, lazy_buffer.memory_used() and the scrollback. (a) memory_used(new(r,c)) ≥ r·c·size_of::<Cell>() > 0, monotone in r·c through the page totals. (b) drop_resize_undo leaves pages.total_memory() unchanged and lowers memory_used by exactly size_of::<ResizeUndo>() + entries.capacity()·size_of::<UndoRow>() + the heap and retained-extras bytes of each Demoted or Pushed row (at least the image payload, the hyperlink bytes and the row's cells). (c) A change to the lazy buffer alone changes memory_used() by exactly the change in lazy_buffer.memory_used(), so staged lines carry their per-line heap bytes.	5	saturating arithmetic over many owned structures, capacities, cross-crate scrollback memory, running sums	0,1,2,4,7,12
history-link-budget	invariant	Grid::{history_link_bytes,history_link_budget,set_scrollback_memory_budget,memory_used}; RingExtras; LazyBuffer::link_bytes	∀ g∈Reach: history_link_budget(g)=memory budget/4 (DEFAULT_MEMORY_BUDGET/4 by default), history_link_bytes(g) ≤ history_link_budget(g), and the distinct URL bytes reachable from history rows (ring, staged, tiered) are ≤ the budget. Over budget, links are removed whole rows at a time oldest first; rows keep their text; stripping stops at a low-water mark (more than budget/2 kept); memory_used counts the kept links. Lowering the budget sheds at once, increments content_gen, and marks full damage when a viewport row loses a link (no stale linked row is served later); raising restores nothing. A workload under the budget keeps every link. The bound survives a width reflow and the re-attach of an offloaded width change.	6	Arc<str> identity, HashSet, running byte sums, loops over rows	0,1,2,3,4,5,7,12
deferred-line-equals-eager	equivalence	DeferredLine::{new,to_line,into_line,into_line_and_body}; Grid::{row_to_line_with_stored_extras,row_to_line_with_stored_extras_at_len}; LazyBuffer::{push,drain_all}; scroll_convert::{append_plain_run,cell_attrs_of}	∀ rows R (ASCII, styled, RGB overflow, complex, combining, wide + continuation, wrapped) and extras X: DeferredLine::new(R,X,v).to_line() and .into_line() equal Grid::row_to_line_with_stored_extras(R,X) on text, wrapped flag, hyperlink spans and per-character attrs indexed by char. The attrs Rle is None iff every cell has default attrs; otherwise run_count = the number of maximal runs of equal attrs, a default prefix or tail kept as its own run. The chunked plain-run fast path (empty extras, len=n) gives text = concat of cell.char() over non-spacer cells (invalid units and COMPLEX-without-extra as U+FFFD), attr runs of CellAttrs(fg or DEFAULT_FG, bg or DEFAULT_BG, flags), is_wrapped=w, and serialize_lines bytes equal to the per-cell reference loop (to become a spec-only fn). LazyBuffer push then drain_all yields the same lines FIFO and leaves the buffer empty.	19	Vec<Cell> snapshot, Rle building loop, span vectors, Arc<str>, VecDeque, chunked slice fast path	0,1,3,5,7,12
lazy-buffer-pool-and-sums	contract	LazyBuffer::{push_row,drain_all,drain_front,drop_oldest,shed_oldest,front_rows_over,staged_bytes}; Grid::resize	∀ LazyBuffer B, after every op staged_bytes(B) = Σ over staged rows of size_of::<DeferredLine>() + occupied_cells·size_of::<Cell>() (running sum = a fresh walk). push_row reuses a pooled body when one exists (pooled -1) and the line equals one built on a fresh body. drain_*, drop_oldest and shed_oldest return bodies to the pool; pool depth ≤ POOL_MAX_BUFFERS and pooled cells ≤ the pool cell budget. drop_oldest(k) removes and returns min(k,staged), adds it to cut_dropped, and len() then counts one marker row. front_rows_over(budget,keep) = the least n such that staged_bytes after shedding n front rows ≤ budget, capped so len-n ≥ keep (0 if it already fits). A column-changing resize empties the pool, a rows-only resize keeps it. Recycling never changes history content.	7	VecDeque<DeferredLine>, Vec<Vec<Cell>> free list, running usize sums, loops	0,1,2,3,5,7
lazy-spine-slack-bound	contract	LazyBuffer::release_spine_slack	f=max(len, DRAIN_THRESHOLD). After release_spine_slack: if capacity > 4f before, shrink_to(2f) was called; else capacity unchanged. The bound capacity' ≤ 4f the test asserts holds only given an upper bound on VecDeque::shrink_to that std does not specify.	1	VecDeque capacity, std allocation behaviour	0,1,2,3,6,12
compress-offload-staging	contract	Grid::{set_compress_offload_active,drain_lazy_bounded,lazy_backlog_len,ASYNC_COMPRESS_BACKPRESSURE}; scroll_convert::FLOOD_MARKER_PREFIX	∀ tiered grids. With offload active, scroll-off lines are staged in the lazy buffer (no inline drain), and while staged the history read oldest to newest is exactly the lines fed. drain_lazy_bounded(k) with backlog m>0 promotes between 1 and min(k,m) of the oldest staged lines into the tiers, returns the new backlog (= lazy_backlog_len()), and leaves the history sequence and count unchanged; while the store is detached for reflow it is a no-op returning m. Under sustained overload lazy_backlog_len() ≤ ASYNC_COMPRESS_BACKPRESSURE (drop-oldest) and the history reads as one marker row naming exactly how many lines were dropped before the first survivor, then a contiguous newest suffix. With offload inactive the reader drains inline and the backlog stays ≤ the 1000-line threshold.	5	&mut self, VecDeque, tier pushes (compression), loops, marker formatting	0,1,2,3,4,5,7,12
flood-backlog-cap	contract	Grid::flood_overflow; LazyBuffer::front_rows_over	F=ASYNC_COMPRESS_BACKPRESSURE. flood_overflow(g) = 0 if staged ≤ F; staged-F with no store; else max(staged -sat max(share,F) (0 with no line limit), front_rows_over(budget-used,F)), where front_rows_over(b,F) is the least n ≤ staged-F whose oldest n rows' staged_bytes bring the running sum (occupied cells, not window width) to ≤ b. Hence after each push the staged rows fit the memory headroom whenever staged > F.	2	&self over VecDeque<DeferredLine>, loop, store queries	0,1,3,4,6,7,12
flood-history-no-silent-hole	invariant	Grid::{drop_flood_overflow,drain_lazy_bounded,scroll_up,truncated_lines}; LazyBuffer::{shed_oldest,drop_oldest}	∀ feeds of numbered lines with any interleaving of drain_lazy_bounded/trickle_lazy_bounded: H(g) is contiguous in line numbers except at flood-marker rows; each marker's count equals the numbering gap across it, measured from the line-limit window's top (origin=max(0,fed-limit)); the counts sum to truncated_lines(); the history ends at the newest scrolled line. Rows the configured limit would evict anyway go store-first, oldest first, uncounted and unmarked (renumber epoch unchanged); only rows inside the limit that the memory headroom cannot hold are cut, marked (DIM) and counted (epoch bumped); with no line limit every drop is a loss; a marker shrinks or vanishes as the window slides past its hole while the counter keeps its peak; a drain promotes the marker first and a later drop opens a new cut behind the promoted rows. Every staged line dropped by backpressure increments truncated_lines by 1; a user limit shrink never changes it.	11	&mut self, VecDeque, tiered store truncate, u64 counters, logging	0,1,2,3,4,5,6,7,8,12
trickle-skip-rule	contract	Grid::{trickle_lazy_bounded,drain_lazy_bounded}; LazyBuffer::{take_arrivals,cut_open}	trickle_lazy_bounded(k) = { a := take_arrivals(); if cut_open ∧ a ≥ k then return len else drain_lazy_bounded(k) }; drain_lazy_bounded also resets arrivals. Consequences: a flood faster than k rows per opportunity leaves one marker per flood (an unconditional drain fragments it), and a slower tail after a flood is promoted and loses nothing.	2	&mut self, u64 compare, early return	0,2,3,4,6
flood-marker-line	contract	scroll_convert::flood_marker_line	∀ d:u64: text = FLOOD_MARKER_PREFIX ++ decimal(d) ++ ' ' ++ (d=1 ? "line" : "lines") ++ " here: output too fast to keep; save it to a file —"; every char's attrs carry DIM; for d ≤ 9_999_999 the text is at most 80 chars; the first token after the prefix parses back to d.	1	format!, String, Rle run	0,1,3,6
extract-row-extras-gate	contract	Grid::{extract_row_extras,extract_row_extras_into}; CellExtras::has_any_data	∀ row i of g: extract_row_extras(row,extras,i,styles) equals the per-cell extraction (map entry, else ring rgb/complex at (i,j)) over every column. So a row whose only datum is ring RGB extracts it though the map is empty, and ring data on other rows never changes a row's result.	2	&self, Vec pushes, ring lookups, gate branches	0,1,3,5,6,7
extract-row-extras-spans	contract	Grid::{extract_row_extras,set_cell_image,row_to_line_with_stored_extras}; CellExtras::any_image	∀ R, E: extract_row_extras(R,E,r,S).hyperlinks = the maximal runs of physical columns (a wide head's spacer belongs to its head's run) with equal (url,id), as [start_col,end_col) in column order; unlinked columns give no span. .images = the maximal runs of columns whose ImageRef shares the same Arc payload (ptr_eq) and footprint row with consecutive footprint columns, each recording (start_col,end_col,image_row,first_cell_col), computed over the full width even when R.len()=0. The image scan runs only when E.any_image(); any_image() is false for a fresh map and after text-only writes and becomes (and stays) true after any set_cell_image. The resulting Line answers get_hyperlink(col) and hyperlink_count accordingly.	6	run-length grouping loop, Arc pointer equality, Option<Arc<str>> comparisons, span Vecs, sticky bool	0,1,3,4,5,7,12
row-to-line-colour-resolution	contract	Grid::{extract_row_extras,row_to_line_with_stored_extras}; StyleTable::get	∀ cell at column k of R: a StyleId present in S gives rgb_fg[k]/rgb_bg[k] = the style's resolved colours and a Line attr with the matching PackedColor::rgb; a StyleId missing from S gives no rgb entry and DEFAULT_FG/DEFAULT_BG; an RGB-tagged cell with no entry in X for k gives DEFAULT_FG/DEFAULT_BG, never the 0x01_000000 placeholder; indexed and default colours pass through unchanged.	3	u32 tag tests on PackedColor, Option lookup into a StyleTable Vec, (u16,[u8;3]) vectors, loops	0,1,3,4,7,12
scrolled-row-extras-is-empty	contract	ScrolledRowExtras::is_empty	∀ x: x.is_empty() ⇔ hyperlinks, complex_chars, combining, rgb_fg, rgb_bg, underline_colors and images are all empty.	1	struct of Vec/SmallVec fields, conjunction	0,1,3,6
row-line-roundtrip	round-trip	Grid::{extract_row_extras,row_to_line_with_stored_extras,fill_row_from_line,history_line_rev,get_history_line,materialize_scrollback_row_full}; scroll_materialize::materialize_from_line	∀ R with WF(R), extras E, style table S, width c ≥ R's occupied width: Mat(L(R),c) and fill_row_from_line(L(R),c) reproduce R cell for cell and extra for extra modulo the normal form N(R). N(R) resolves StyleId cells to explicit colours; drops wide spacers and re-derives them from their lead (WIDE_CONTINUATION plus the lead's rendition flags, the lead's underline colour in packed form so an indexed colour stays an index, the lead's fg/bg); keeps PROTECTED cells (bit 10 shared with the spacer); keeps complex chars, combining marks, ZWJ clusters, RGB fg/bg and hyperlink url+id at their physical columns, images as per-cell refs sharing the same Arc payload with footprint row/col, the write-time width (stored WIDE bit, not a re-derived East-Asian width), visual flags, the wrapped flag, and len (an image-only row has len 0 and text ""). The scroll-off path stores exactly L(R), so every history consumer (history_line_rev, materialize_scrollback_row_full, unscroll) reads N(R) back.	27	per-cell String building, Rle<CellAttrs> (cross-crate), span Vecs with Arc identity, UTF-8, packed u32 colour tags, loops; content models on both sides of the seam	0,1,3,4,5,7,12
materialize-from-line-denotation	contract	scroll_materialize::{materialize_from_line,advance_grapheme_unit_wide}; MaterializedRow::{len,is_empty}	∀ Line L, c≥1: the result has exactly c cells placed by walking Units(L.text) left to right. A unit takes 2 columns when its stored WIDE attr is set or by its effective width (VS16 widens an emoji-capable base, VS15 narrows; an unfolded text-presentation base plus a Fitzpatrick modifier lays out as narrow base then wide modifier), else 1. A unit of more than one scalar or a non-BMP scalar gets complex_char=the unit and COMPLEX; a wide unit's next cell is its continuation. A VS16 unit whose base sits in the last column stays narrow and present. A wide unit that cannot fit at the last column stops the scan: it and every later unit are dropped and those cells stay default. An RGB-tagged attr gives fg_rgb/bg_rgb extras (default colours give none), and a dropped unit's attrs never shift onto earlier cells. Hyperlink spans [s,e) give hyperlink+id on [s,min(e,c)). Cells beyond the text are default. len() = 1 + the last column holding a non-space glyph or non-default attribute (trailing default spaces do not count, coloured ones do), or 0; is_empty() ⇔ len()=0.	24	&str byte indexing and char decoding, Rle iteration, Vec<Cell> of length c, per-column Vec<Option<CellExtra>> with Arc<str>, Unicode width tables (cross-crate), loop with early break	0,1,3,4,5,7,12
line-consumers-agree	equivalence	Grid::fill_row_from_line; scroll_materialize::{materialize_from_line,materialize_from_row_extras}	∀ Line L, c: fill_row_from_line(row,L,c) writes into grid row `row` exactly the cells of materialize_from_line(L,c) and the map entries of its extras (hyperlink id included), with HAS_EXTRAS set exactly on cells holding an entry. ∀ R, X with materialize_from_row_extras(R,Some(X),c)=Some(m): m = materialize_from_line(row_to_line_with_stored_extras(R,X),c), including zero-length (image-only) rows. So ring-tier and tiered reads of the same history agree.	3	three implementations over the same data (loop equivalence), &mut self, HashMap extras, Arc	0,1,2,3,4,5,7,12
fill-row-from-line	contract	Grid::fill_row_from_line; fill_row_into; fill_row_cells; filled_span_cols	∀ g, r, line, cols: r≥rows ⇒ fill_row_from_line(r,line,cols) changes nothing. Otherwise row r is cleared and the grapheme units of line.as_str() are placed left to right by effective width with their attrs until cols is reached (the rest truncated); a line with no text leaves the row blank; the wrapped flag = line.is_wrapped(); combining marks and complex chars go to their cells' extras. Hyperlinks and underline colours are restored only on the columns in filled_span_cols (cells holding a glyph), with at most cols column-writes in total, so with overlapping spans the first wins. Other rows unchanged. fill_row_into on a fresh row returns a ScrolledRowExtras with the same spans restricted to the filled columns.	11	&mut self, &str grapheme iteration, Vec<DeferredExtra>, CellExtras writes, Row cells (unsafe pages)	0,1,2,3,5,7,8,12
stored-cells-run-expansion	equivalence	stored_line_cells; StoredCells::{new,next}	∀ line, text=its text, cols≥1: expanding every (bytes,col,width,count) run of stored_line_cells(line,text,cols) into count cells (byte=bytes.start+k·len, column=col+k·width, wide=(width=2)) gives exactly the per-cell sequence (byte,text.len,col,wide) of StoredCells::new(line,text,cols), with or without a stored WIDE attr.	1	iterators over &str, Rle attr lookup, loops	0,1,3,5,7,12
grapheme-unit-segmentation	refinement	scroll_materialize::advance_grapheme_unit_wide; advance_grapheme_unit (cfg(test) wrapper)	∀ t:&str and char boundary i<|t|, u=advance_grapheme_unit_wide(t,&mut i) advances i to a boundary i'>i with i'-i ≤ 256+4. t[i..i'] is the unit the live writer folds into one cell: a base, then combining marks, variation selectors and Fitzpatrick modifiers (a modifier folds only when the effective width so far is wide), with a ZWJ joining the next scalar unless it is printable ASCII. A regional indicator folds onto a unit whose base is a lone regional indicator, even past marks, and a third indicator starts a new unit. u.chars = scalars consumed; u.wide = effective width 2 (base default width, VS16 widening an emoji-capable narrow base, VS15 narrowing a wide emoji, a standalone modifier wide). Iterating from 0 terminates after at most |t| calls and consumes exactly |t| bytes. The reference is the live writer's fold rules, which should be shared, not duplicated.	10	&str, char iteration with byte offsets, &mut usize out-param, Unicode property tables (cross-crate), while loop with a byte-ceiling variant	0,1,2,3,5,7,12
grapheme-ascii-fast-path	equivalence	advance_grapheme_unit_wide; advance_grapheme_unit_scan; is_zero_width	∀ t:&str, byte index i at a char boundary: advance_grapheme_unit_wide(t,&mut i) = advance_grapheme_unit_scan(t,&mut i) on chars, wide, vs16_widened, force_narrow and the advanced index. Lemmas: for ASCII c, is_zero_width(c) ⇔ char_width(c)=0, and char_width(c)=1 for printable ASCII.	1	&str byte scanning loops, char width tables (cross-crate), struct result	0,1,3,5,7,12
checkpoint-visible-roundtrip	round-trip	Grid::{checkpoint_visible_lines,restore_visible_from_lines,checkpoint_lines_bounded,extract_row_extras,row_to_line_with_stored_extras_at_len,fill_row_from_line}	∀ g with Inv(g) and h with the same rows and cols: after h.restore_visible_from_lines(&g.checkpoint_visible_lines()), Obs(h,r)=Obs(g,r) for every screen row r and row_text agrees. A missing line clears the row, so a restore with fewer lines than rows blanks the rest. (The tests drive #[cfg(test)] checkpoint_lines = checkpoint_lines_bounded(usize::MAX); retarget them.)	7	Vec<Line>, String, per-cell extras conversion through aterm_scrollback (cross-crate), loops	0,1,2,7,12
checkpoint-lines-layout	contract	Grid::{checkpoint_lines_bounded,checkpoint_visible_lines,checkpoint_lines}	∀ g, m: checkpoint_lines_bounded(g,m) = H[|H|-k..] ++ checkpoint_visible_lines(g) with k=min(m,scrollback_lines); an unreadable history line becomes Line::new(). checkpoint_visible_lines(g) has exactly rows() lines and reads the live screen (row_at_screen), so scroll_display does not change it. Restoring visible lines into a fresh grid adds no history. Rebuilding a tiered scrollback from the history prefix of checkpoint_lines, then restoring the visible suffix, reproduces scrollback_lines and every history line in order.	3	Vec<Line>, iterator take/map, Cow, cross-crate scrollback	0,1,2,7,12
reflow-mode-from-bool	contract	ReflowMode::from	ReflowMode::from(b) = if b { Enabled } else { Disabled }.	2	enum result, if	0,1,3,6
resize-clamp-and-reset	contract	Grid::{resize,resize_no_reflow,resize_with_reflow_mode,resize_with_reflow_mode_and_policy,restore_cursor}; GridStorage::resize_viewport_state	∀ g∈Reach, r, c, mode, policy: after any resize, rows()=clamp(r,1,MAX_GRID_ROWS), cols()=clamp(c,1,MAX_GRID_COLS) and Inv(g'). The cursor row is < rows and column < cols (clamped when the reflow does not move it). Scroll region and margins are full, has_horizontal_margins=false, pending_wrap=false. tab_stops per tab-stops-resize-grow-only. saved_cursor is unchanged by the resize (restore_cursor clamps it). A same-size resize changes neither content nor cursor. Cells in rows or columns beyond the previous extent read blank, including after a shrink-then-grow, at multi-page widths and with a rotated ring.	18	&mut self, Vec<bool> resize, ring re-layout, page allocation (unsafe)	0,1,2,3,4,7,8
reflow-rewrap-layout	refinement	Grid::{reflow_columns,reflow_rewrap_columns,merge_continuation_rows,finalize_reflow,resize}; reflow_map::{chunk_cells_to_rows,adjust_chunk_boundary,copy_cells_to_row}	∀ g∈Reach, c'≠cols(g), resize(g,r,c') with ReflowMode::Enabled, Native maps every single-size logical line L of the live grid (Logical, §0) to exactly the rows Chunk_{c'}(L), in order, before placement (reflow-viewport-placement). Every surviving cell keeps its char, colours, flags (PROTECTED included, though it shares bit 10 with WIDE_CONTINUATION; a spacer is skipped only after a replaced wide head) and complex char. Hard breaks never merge; empty rows stay empty rows; DECDWL/DECDHL rows resize in place (double-size-rows-resize-in-place). The loop terminates for every c'≥1. Corollaries: concat of Logical(g') = concat of Logical(g) up to the c'=1 wide-head blanking and the retention caps, so content written before a shrink/grow sequence is still present; c'=cols with rows unchanged ⇒ Screen'=Screen and cursor unchanged; afterwards every row is writable and readable at the new geometry.	29	Vec<Row>/slices of packed Cell, while loops with a back-off step (decreases: chunk ≥1 after back-off), wide-flag bit tests, unsafe Row::new, extras copy ctx; needs a Lean list-chunking model	0,1,2,3,4,5,7,8,10,12
reflow-cursor-tracking	contract	Grid::{reflow_columns,reflow_rewrap_columns,finalize_reflow,lend_saved_cursor,take_lent_saved_cursor}; RewrapCursors::{points,land,place_on_row}; reflow::cursor_row_copy_len; reflow_map::cursor_in_chunk	Let the cursor (and a lent saved cursor) sit at logical offset o of logical line L (cells of L's earlier rows + c). Native: after a reflow to c', the cursor is at the coordinate of offset min(o,|content|) in Chunk_{c'}(L), adjusted by the rows placement moved to history; an offset equal to a chunk's end goes to the next chunk except on the last chunk, where it stays at the end (cursor_in_chunk's ≤ rule); an offset on the spacer of a c'=1 replaced head lands on the blank; an offset past L's content keeps its column relative to the row, clamped; finally col < c' (and < c'/2 on a double-width row). For a single-row o<c' with no wide cells the result is (row(L)+o div c', o mod c'), so a shrink then grow back returns to (r,c) when o ≤ |content|. ConPty: the cursor row is copied through cursor_row_copy_len=min(max(len,c+1),C) cells, so the cursor keeps o exactly: when the cells before it end on a row boundary its cell starts a WRAPPED row of its own; a cursor on a blank row past new_cols moves into a WRAPPED row below; a width round trip restores (r,c) and an empty screen stays empty.	10	[TrackedPoint;2] with Option<usize> offsets, integer div/mod, min clamps, &mut self; depends on the chunking lemma	0,1,2,3,4,5,6,7
reflow-viewport-placement	contract	Grid::{finalize_reflow,restore_reflowed_scrollback,trailing_blank_rows_below_cursor,fill_viewport_deficit_from_history,invalidate_pending_fill_target,reattach_reflowed_scrollback}	A width reflow produces N rows with the cursor on row k; T = the trailing blank rows below the cursor on content rows at display_offset 0 before the resize (independent of display_offset). (a) N>r ⇒ the minimal top prefix needed to keep row k visible (after trimming trailing blanks below the cursor) goes to history, which then reads [rewrapped old history | that overflow in order | window-staged lines] in write order. (b) Native and not ConPTY: newest history rows are pulled back into the viewport top until the blank band below the cursor equals T again, so the prompt returns to its pre-resize row with no blank band above it, and a genuine erased band (counted in T) is never refilled. (c) A content mutation that invalidates the target during an offload window (ED 2, SU, DL...) cancels the fill at re-attach, so cleared rows stay blank and no history is resurrected into them.	5	&mut self, Vec<Row> splits, lazy staging, ring/tiered push (cross-crate), Option<usize> fill target, loops	0,1,2,3,4,5,7,8,12
width-change-seam-policy	contract	Grid::{sever_top_row_continuation,take_boundary_continuation_lines,fill_viewport_deficit_from_history,restore_reflowed_scrollback}	∀ g∈Reach, width change with reflow: ConPty: the history is rewrapped by itself (no boundary lift, no deficit fill), row 0's WRAPPED flag is cleared before the rewrap, and screen rows rewrap in place; hence over a ConPty width round trip H and scrollback_lines are unchanged. Native: the screen's leading WRAPPED rows are joined to their history head before the history rewrap, and the viewport is bottom-anchored by pulling history back.	3	&mut self, Vec<Line>, store reads/writes	0,1,2,3,4,5,6,7,12
conpty-width-demote	contract	Grid::{conpty_reflow_demote,finalize_reflow}	conpty_reflow_demote(res,t) = min(max(1 + last row that is non-empty or wrapped (0 if none), res.cursor_row+1) -sat t, res.cursor_row). A ConPty width change demotes exactly that many top rewrapped rows to history and keeps every other content row on screen, including non-blank rows below the cursor; a blank row under the cursor is dropped instead, after which Native and ConPty agree.	2	Vec<Row> scan (rposition), usize arithmetic	0,1,3,5,6,7
double-size-rows-resize-in-place	contract	Grid::{reflow_rewrap_columns,merge_continuation_rows}; Row::{set_line_size,resize}	∀ row R with line_size(R)≠SingleWidth and every width reflow to c': R maps to exactly one output row with the same line_size and wrapped flag, whose cells are R's truncated to the row's usable width (cells past it dropped; a wide head left without its continuation at the new edge blanked). R is never split into continuation rows, never absorbs its successor or predecessor (even as a wrap continuation), and does not grow the row count. Neighbouring single-width lines reflow independently (reflow-rewrap-layout); the cursor on R is clamped to the new width.	11	enum match on LineSize, Row::resize over unsafe pages, Vec<Row>, loops	0,1,2,3,4,5,7,8
reflow-extras-follow-cells	contract	reflow_map::copy_cells_to_row (ExtrasCopyCtx); Grid::reflow_columns; CellExtras::{get_or_create,set,get}	∀ width reflow and extras entry e at old (row,col) whose cell is at logical offset o of L: if the cell survives, the new grid has the same entry (hyperlink+id, underline colour, fg/bg RGB, complex char) at the coordinate of o in Chunk_{c'}(L) with HAS_EXTRAS set; if it is a c'=1 replaced wide head, the entry is dropped and the blank's HAS_EXTRAS is clear. No other coordinate gains an entry, so afterwards HAS_EXTRAS ⇔ entry (both directions) and no stale entries remain at old coordinates.	8	HashMap<CellCoord,CellExtra> rebuild, Arc<str>, [u8;3], loops; a map-valued content model	0,1,2,3,4,5,7
remap-copied-extra	contract	reflow_map::{remap_copied_extra,source_coord}	∀ old extras E, source s, i, (dr,dc), k=source_coord(s,i): old_extras=None or k=None ⇒ new extras unchanged. Else if E has a stored entry, a ring complex codepoint or ring fg/bg at k: new_extras[(dr,dc)] = the stored entry with all its fields, complex := the ring codepoint only when the entry has none, fg/bg := the ring values when present (ring first, as rendered); otherwise nothing is set.	1	&mut ctx struct, Option chains, Arc<str>, HashMap	0,1,2,3,6
adjust-chunk-boundary	contract	reflow_map::adjust_chunk_boundary	∀ cells, o ≤ e ≤ |cells|: a = (e<|cells| ∧ e>o ∧ WIDE(cells[e-1])) ? e-1 : e; result = (a=o ∧ o<|cells|) ? o+min(2,|cells|-o) : a. So result>o whenever o<|cells| (progress), result ≤ |cells|, and a chunk never ends on a WIDE head whose continuation is outside it unless progress forces it.	7	pure fn over &[Cell], indexing, flag test, min	0,1,3,6
cursor-in-chunk	contract	reflow_map::cursor_in_chunk	∀ off,n,s,e,d: x=min(off,n); result = Some((d,x-s)) if x≥s ∧ (e=n ? x≤e : x<e), else None.	8	pure scalar fn returning Option<(usize,u16)>, min, row_u16 call	0,1,3,6
source-coords-for-row	contract	reflow_map::source_coords_for_row	∀ row, len: |result|=len and result[i]=CellCoord::new(row,i) for every i<len.	4	Vec collect over a mapped range	0,1,3,5,6,7
copy-cells-to-row	contract	reflow_map::copy_cells_to_row	∀ cells, o ≤ e ≤ |cells|, content_len, new_cols: walking cells[o..e] with destination j from 0: a WIDE cell when new_cols<2, or a WIDE cell at j=new_cols-1, writes Cell::EMPTY at j and skips its continuation; otherwise while j<new_cols the cell is written at j (extras per remap-copied-extra) and j+=1. Len' = 1 + the last j written from a source index < content_len that is not a blank image tile (0 if none). e=o leaves the row empty.	4	&mut Row, slice iteration with a small state machine	0,1,2,3,5,6,7
chunk-cells-to-rows	contract	reflow_map::chunk_cells_to_rows	∀ cells, content_len, new_cols≥1, points: the rows appended are the chunks [o_k,o_{k+1}) with o_0=0, o_{k+1}=adjust_chunk_boundary(cells,o_k,min(o_k+new_cols,|cells|)), each filled by copy_cells_to_row; rows k>0 have Wr=true, row 0's flag is the caller's; a row has EARLY_WRAP_HOLE iff o_{k+1} < min(o_k+new_cols,|cells|); every point with offset o lands at cursor_in_chunk(o,|cells|,o_k,o_{k+1},base+k) of its chunk (keeps_gap points past the content keep o-o_k). A printed space before content_len counts in Len. This is the code-level definition of Chunk_c.	7	while loop, Vec<Row> push, unsafe Row::new, &mut [TrackedPoint]	0,1,2,3,5,6,7,8
reflow-history-rewrap	contract	Grid::{take_scrollback_lines,take_boundary_continuation_lines,restore_reflowed_scrollback}; scrollback_reflow::reflow_scrollback_lines; ScrollbackStorage line limit	For a width change with reflow, the logical history lines (H joined on wrapped, including the boundary-straddling line lifted from the viewport top) after the resize are exactly those before it, in order, each re-chunked by Chunk_{c'}; the physical history count is Σ_L |Chunk_{c'}(L)| (grows on a shrink, shrinks on a widen). With a store line limit l the restore keeps the newest l physical lines. Corollary: c→c'→c on lines written at width c restores scrollback_lines() exactly.	4	Vec<aterm_scrollback::Line> (cross-crate), loops, store push with limit enforcement	0,1,2,3,4,5,7,12
scrollback-rewrap-denotation	contract	scrollback_reflow::{reflow_scrollback_lines,reflow_scrollback_lines_reference,reflow_newest_rows,emit_logical_line,rewrap_passthrough_eligible}	∀ L:[Line], w, w'=max(w,1): split L into logical runs (a head plus following wrapped lines; a leading orphan continuation opens a run). reflow_scrollback_lines(L,w) = concat over runs of split(U,w'), U the run's display units. A unit's width follows the stored WIDE attr and the VS15/VS16 rules (a stored-wide ambiguous char is 2; VS15 narrows despite WIDE). split is greedy by display width with no unit straddling a row; the first output row is unwrapped and the rest wrapped; an empty run gives one empty line. Each unit keeps its text, attrs, hyperlink (url,id) and underline colour; columns in the gaps between spans carry none. Corollaries: hard newlines preserved; reflow(reflow(L,a),b) has the same logical lines; the passthrough clone is taken iff run.len()=1 ∧ rewrap_passthrough_eligible and is semantically equal to reflow_scrollback_lines_reference(L,w); reflow_newest_rows(L,w,k) = the last k rows of reflow_scrollback_lines(L,w) and rewraps only the newest runs it needs.	15	slices of Line, Vec<Unit> scratch, Rle attrs, span vectors, loops, char width tables	0,1,3,5,7,12,14
width-roundtrip-restores-topology	round-trip	Grid::{resize,resize_offloading_scrollback,reattach_reflowed_scrollback}; PendingScrollbackReflow::reflow	Call g canonical at width c when every element of Transcript(g) is a row of Chunk_c of its logical line as autowrap at c would lay it out (early-wrap holes included). ∀ canonical g and every sequence of width resizes w1..wk=c (direct on a ring-only or tiered grid, or each offloaded with an immediate reattach): Snapshot after = Snapshot(g); extras and RGB return to their original coordinates; no phantom space appears at an early-wrap seam, whether in history, the viewport or across the boundary. Corollary: the offloaded and the direct step give equal Snapshots, independent of display_offset at each step (the reader's position is offload-reader-position).	8	composition of reflow-rewrap-layout, reflow-history-rewrap and reflow-viewport-placement; Chunk_c(join(chunks))=chunks lemma; Line↔Row seams	0,1,2,3,4,5,7,8,12
history-renumber-epoch-on-width-reflow	contract	Grid::{resize,history_renumber_epoch}; scrollback_reflow (epoch bump); reflow_pullback; history_carry	history_renumber_epoch never decreases (saturating_add). ∀ g, (r,c): c≠cols(g) and the width reflow changes the absolute index or text of any row (whether or not a row crossed into history) ⇒ epoch(resize(g,r,c)) > epoch(g). c=cols(g) ⇒ epoch unchanged, for a rows-only change and a same-size call, unless a pullback or unscroll path ran. Semantic form: for every absolute-row-keyed cache key (absolute_row, epoch), an unchanged epoch across any Grid op means that absolute row names the same text as before.	2	Grid &mut self two-state on a u64 counter, reflow loops, scrollback Lines	0,1,2,3,7
no-reflow-resize-truncates	contract	Grid::{resize_with_reflow_mode,drop_unclaimed_extras}; Row::resize; CellExtras::retain_cols_below	For a resize with ReflowMode::Disabled to (r,c'), each visible row R becomes Row::resize(R,c'): cells [0,min(len,c')) kept unchanged (flags such as PROTECTED included); a wide head whose continuation falls at or past c' is replaced by an empty cell. Wrapped flags are untouched: nothing wraps or unwraps and no row is merged or split. Extras with col ≥ c' are dropped, along with the entry of a blanked head. The row count then follows rows-only-resize-transcript.	6	unsafe Row::resize over a fresh PageStore, loops over rows, HashMap retain	0,1,2,3,4,6,7,8
resize-keeps-reader-position	contract	Grid::{resize_with_reflow_mode_and_policy,scroll_to_absolute_row,top_visible_absolute_row}	p = display_offset before a resize. The resized live content equals what the same resize produces from the same grid at display_offset 0. Afterwards: p>0 with unchanged width ⇒ the absolute row that was at the top of the view is again at the top (scroll_to_absolute_row(anchor), clamped to [0,scrollback_lines()]); otherwise display_offset=min(p,scrollback_lines()). A scrolled-back reader is never snapped to 0 by a direct resize unless history shrank to 0.	1	Option<u64> anchor, u64/usize arithmetic and min	0,1,2,3,4
rows-only-resize-transcript	refinement	Grid::{adjust_row_count_rows_only,adjust_row_count,keep_keys_across_bottom_rows,inject_scrolled_extras}; GridStorage ring reclassification	Resize with c'=cols(g) (either mode) from r to r', cap = max_scrollback (ring) or the total limit (tiered). Shrink: trailing blank rows below the cursor are trimmed first (never archived), then top rows are demoted to history in order until r' rows remain with the cursor row visible; with the cursor on the top row the bottom rows are pushed instead and no row is lost; history keeps its newest cap lines (cap 0: nothing retained). Grow: the newest min(r'-r, scrollback) history rows are revealed at the top, then blank rows appended. Same size: identity. In every case the transcript of non-blank rows (cells, extras, wrapped flags, non-BMP scalars) is unchanged and in write order, the cursor stays on its content line (shifting down by the revealed rows), and (H,Rows,oldest_absolute_row) is identical on a ring-only grid and a tiered grid fed the same writes. Corollary: a shrink then the inverse grow with nothing written between is the identity on Rows, cursor and H.	17	ring reclassification (modular ring_head, total_lines), ring_extras side table, lazy staging, tiered store, cursor/saved-cursor shifts; a two-implementation equivalence	0,1,2,3,4,5,7,8,12
rows-grow-policy	contract	Grid::{adjust_row_count,adjust_row_count_rows_only,keep_keys_across_bottom_rows,take_last_resize_row_shift}	∀ g∈Reach, rows-only grow by d: Native: revealed=min(d,scrollback_lines) newest history lines become rows 0..revealed, the cursor moves down by revealed, take_last_resize_row_shift()=revealed, history_reveal_gen'=gen+(revealed>0), and d-revealed blank rows are appended. ConPty: revealed=0, rows 0..R unchanged, d blank rows appended, cursor, history and reveal_gen unchanged. Both: counter'=counter+appended, so oldest and visible_to_absolute(0) are unchanged and history_renumber_epoch is unchanged; a following ConPty shrink by d trims exactly the appended blanks and restores counter.	3	&mut self, Vec splice, ring linearize	0,1,2,3,4,6,7
rows-shrink-shape	contract	Grid::{adjust_row_count_rows_only,last_resize_shape,take_last_resize_shape}	∀ g∈Reach, rows-only shrink by d: trimmed=min(d, blank rows below the cursor); demoted=min(d-trimmed, r); pushed=d-trimmed-demoted; the top demoted rows and the bottom pushed rows leave the screen into history (or, on a retention-0 grid, into the resize undo: stashed counts them); r'=r-demoted. ConPty and Native give the same Screen, H, cursor and renumber epoch. last_resize_shape reports these counts (and revealed/appended for a grow; reflowed = width changed ∧ reflow enabled); take_last_resize_shape returns it and leaves the default; a same-size resize records the default shape.	2	&mut self, struct of counters	0,1,2,3,4,6,7
resize-undo-flap-identity	equivalence	Grid::{admit_resize_undo,seal_resize_undo,take_undo_rows,resize_no_reflow,resize_undo_rows,drop_resize_undo}	∀ g∈Reach with max_scrollback=0 (alternate-screen shape), no tiered store, no history, Native: any sequence of rows-only resizes at fixed width with nothing between that ends at g's height gives Picture(g')=Picture(g) (every visible cell, its extras and RGB, the cursor) and resize_undo_rows()'=0. Shrinks push UndoRow entries in unit-shrink order (Trimmed, Demoted, Pushed); a grow by g pops min(g,|entries|) LIFO (demoted rows back on top as restored_top; pushed rows and trimmed blanks back at the bottom as restored_bottom/appended) and appends blanks for the rest. resize_undo_rows = number of Demoted+Pushed entries.	5	&mut self, Vec<UndoRow> LIFO, Box, unsafe Row::from_snapshot, AtomicU64 serial	0,1,2,3,4,5,6,7,8,13
resize-undo-validity	contract	Grid::{keeps_resize_undo,admit_resize_undo,drop_resize_undo}	The stash survives a resize iff policy=Native ∧ max_scrollback=0 ∧ no staged store ∧ total_lines=R ∧ cols unchanged ∧ rows_after=R ∧ seal_gen=content_gen ∧ after=cursor_picture(); otherwise it is dropped and the resize runs normally (the grow appends blanks and the screen stays shifted). Hence a write, a cursor move, a scroll-region change, drop_resize_undo, a width change, the ConPty policy, or a retaining grid each give resize_undo_rows()=0 and stashed=0.	2	&mut self, struct equality, Option<Box>	0,1,2,3,6
resize-row-reuse-refines-model	refinement	Grid::{resize_no_reflow,resize_with_policy}; PageStore::{stats,recycled_rows,active_pages}; aterm_spec::derive::resize_row_reuse_model (ty_model!)	The projection (live=|storage.rows|, free=recycled_rows, allocated=pages.stats().allocations, peak=row high-water) of every schedule of Shrink, Grow, Rebuild (width toggle) and Output (drop_resize_undo + write) refines resize_row_reuse_model step for step, and OwnedExactlyOnce and BoundedByHighWater hold; a mutant shrink that discards its token breaks OwnedExactlyOnce (negative control). Corollaries: repeated rows-only flaps of any shape and policy (Native demote/push/trim, ConPTY, with or without output) never allocate past the high-water mark (allocations and active page bytes unchanged), leave recycled_rows=0 after the grow when no history was retained, and keep retained ring bodies pooled until later output reuses them.	4	unsafe PageStore with a row free-list, BTreeMap projection, ty_model! interpreter (cross-crate aterm-spec)	0,1,2,3,4,7,8,10,12
truecolor-survives-resize	contract	Grid::{migrate_rgb_ring_to_extras,migrate_complex_ring_to_extras,fg_rgb_at,bg_rgb_at,reflow_columns}; CellExtras::{invalidate_rings,set_rgb_ring_range}	∀ resize (rows-only, same size, Disabled width change, or column reflow) of a grid with or without history: every visible cell of g' that comes from cell x of g (by the row mapping of rows-only-resize-transcript, no-reflow-resize-truncates or reflow-rewrap-layout) has RGB(g',cell)=RGB(g,x), whether x's colour lived in the dense ring or the map. A cell's RGB-overflow flag is never left without its value, across any number of consecutive resizes.	5	dense ring arrays harvested into a HashMap, loops	0,1,2,3,4,5,7
rgb-harvest-cost	cost	Grid::migrate_rgb_ring_to_extras; test_counters::count_rgb_harvest_cells	Ghost counter: one resize that does not column-reflow harvests exactly the number of ring-stored truecolour cells and then drops the ring; so with no RGB ring (no truecolour on screen, or a second resize after the first dropped it) the delta is 0.	2	thread_local ghost counter, Option<ring> state, loops	0,1,2,3,4,7,13
rows-only-resize-cost	cost	Grid::adjust_row_count_rows_only; test_counters::count_rows_only_resize_migrated_rows	Ghost counter: a rows-only resize r→r' migrates at most max(0,r-r') rows out of the ring (0 on a grow), whatever the ring depth and store size. Afterwards ring_buffer_scrollback() ≥ its value before minus that count, and scrollback_lines() does not decrease (shrink) or decreases only by the rows revealed (grow).	4	thread_local ghost counter, loops bounded by the delta	0,1,2,3,4,7,13
reflow-cost	cost	Grid::{reflow_rewrap_columns,merge_continuation_rows,resize}; reflow_map::copy_cells_to_row; test_counters::{count_reflow_row_ops,count_reflow_cell_ops}	Ghost counters: a width reflow visits each logical line of the visible source once: reflow_row_ops = logical lines visited ≤ source visible rows (a shrink or grow of R full unwrapped rows gives exactly R, however many output rows, independent of history length); reflow_cell_ops ≤ k·(source cells + output cells) for a fixed k, independent of the old/new width ratio.	4	ghost counters in nested loops (loop invariants tie the counter to progress)	0,1,2,3,4,5,7,8,13
scroll-off-is-lazy	cost	Grid::{scroll_up,drain_lazy_buffer,row_to_line_with_stored_extras}; test_counters::{count_row_to_line_ops,count_row_to_line_cells}	Ghost counters: scroll_up(n) performs 0 row_to_line conversions (each scrolled row is staged as a DeferredLine) at any grid size and n; drain_lazy_buffer performs exactly one conversion per staged line, independent of the row count; one row_to_line visits each cell of the row's slice exactly once (cols for a full row).	4	ghost counters, DeferredLine staging (VecDeque), loops over cells	0,1,2,3,4,7,13
width-resize-sync-cost	cost	Grid::{resize_with_reflow_mode_and_policy,resize_offloading_scrollback}; test_counters::count_scrollback_reflow_sync_lines; PendingScrollbackReflow::line_count	Ghost counter: a direct width resize adds exactly |take_scrollback_lines()| + |take_boundary_continuation_lines()| ≥ scrollback_lines() to the synchronous rewrap counter. resize_offloading_scrollback on a grid with a tiered store and a width change adds 0 and returns Some(job) with job.line_count() = store + lazy + ring history lines (all three tiers ride the job). After reattach(job.reflow()), history is the rewrapped job content (reflow-history-rewrap).	2	thread_local ghost counter (cfg(any(test, testing))), Option<PendingScrollbackReflow> owning a moved store	0,1,2,3,4,12,13
offload-window-refinement	refinement	Grid::{resize_offloading_scrollback,reattach_reflowed_scrollback,abort_reflow_offload,erase_scrollback,attach_scrollback,set_scrollback_line_limit,set_scrollback_memory_budget,scrollback_line_limit,scrollback_memory_budget,reflow_offload_in_flight,lazy_backlog_len,set_compress_offload_active}; PendingReflow::reflow; drain_lazy_buffer; offload_window_spec.rs	∀ tiered grid with history and a width change that opens the window, and every interleaving of Produce, Erase, SetLine{Low,High,Unlimited}, SetBudget{Low,High}, AttachReplacement, Reattach and Abort: the projection (detached, backend, line/budget observed and applied, ring_limit, relocated, retained) refines offload_window_model (Buggy=0). Hence: NoLoss (on a clean Reattach or replacement-backed Abort every window line is retained or relocated); ErasedStaysErased (no pre-erase line reappears; output written after the clear is kept); pending_scrollback_settings is Some exactly while the window is unresolved; getters expose the newest deferred request (None = unlimited, distinct from no request) and scrollback()=None during the window; dirty settings apply once, latest-writer-wins, when a backend attaches (store limit = total - ring cap); an untouched snapshot adopts the replacement's baseline and the stale job never clobbers the replacement; a backend-less Abort (job finished or half-stepped) leaves scrollback()=None, budget None and a ring capped at min(requested total, construction cap), never expanded; a replacement-backed Abort drains staged rows into it; set_scrollback_memory_budget on an attached store drains staged rows first, so a generous budget loses nothing. Inv holds after each terminal step. The ty check of the model (Buggy=1 counterexample) moves to the verify model lane.	14	&mut self across a detach/reattach protocol, owned job moved across calls (a worker thread in production), Option<ScrollbackStorage>, budgets, model projection	0,1,2,3,4,5,7,8,10,12,13
offload-window-conserves-history	refinement	Grid::{resize_offloading_scrollback,reattach_reflowed_scrollback,abort_reflow_offload,erase_scrollback,adjust_row_count}; PendingScrollbackReflow::reflow; LazyBuffer cap DETACHED_LAZY_CAP	H = history at detach (rewrapped by the job), W = lines scrolled off during the window (a mid-window rows-only resize's demotions are staged, not dropped). On reattach(job.reflow()): H(g') = rewrap(H) ++ takeLast(CAP,W) with no erase_scrollback in the window; with an erase, H(g') = the window lines scrolled off after the erase only (stale H is not resurrected). Every window line dropped past CAP increments truncated_lines. A resize inside the window with unchanged width returns None (no re-detach). abort_reflow_offload discards H and the staged lines and closes the window; afterwards scroll-off is ring-only bounded (scrollback_lines() ≤ ring cap + steady-state store) and nothing accumulates in an undrainable buffer. Inv holds at every point.	5	protocol over two owners (grid and job), generation checks, VecDeque cap with drop-oldest, cross-crate store move; best as a ty_model! protocol plus refinement	0,1,2,3,4,5,7,10,12,13
offload-window-cap-logging	contract	LazyBuffer cap path (scroll.rs, DETACHED_LAZY_CAP); Grid::{reattach_reflowed_scrollback,abort_reflow_offload}	For each window whose staging exceeded CAP with k≥1 dropped lines, exactly two WARN records are emitted on that thread: one at the first drop ("reflow window: lazy buffer exceeded 50000 lines...") and one when the window closes by reattach or abort ("reflow window: closed after dropping k staged line(s) past its 50000-line cap"), with k = the window's truncated_lines delta. A window counts from 0; a window with no drop logs nothing.	1	aterm_log effect (global logger, thread_local capture), format!, per-window u64 counter and first-drop latch	0,1,2,3,4,8,12,13
offload-step-schedule-independence	equivalence	PendingScrollbackReflow::{reflow,reflow_step,lines_done,lines_total}; reflow_scrollback_lines	∀ jobs J from resize_offloading_scrollback and budget schedules (b_i), each clamped to max(b_i,1): stepping terminates; lines_total is fixed at detach (=line_count); lines_done is monotone, ≤ total, and reaches total on Done; while InProgress the carry holds ≤ b + one logical run. The Done result equals reflow(J) (store content, order, Debug fingerprint, ring_out), so a step boundary never splits a soft-wrapped run, and an empty history steps to Done with 0 lines. Without a binding byte budget (any line limit) the store equals the materialize-all reference (reflow_scrollback_lines over store++lazy, then a push with the limits active). Under a binding byte budget the reference is the newest suffix of the streamed store, |streamed| ≥ |reference|, both under budget. The limits are restored after the flight.	8	loops over a moved job, enum state machine, Vec<Line>, store truncate/push, budget arithmetic	0,1,2,3,4,5,7,10,12
offload-refines-plain-resize	refinement	Grid::{resize_offloading_scrollback,resize_offloading_scrollback_with_policy,reattach_reflowed_scrollback,reattach_reflowed_scrollback_or_redetach,resize_with_policy}	∀ tiered grids, every drag (a first resize that detaches, further resizes inside the window, output between), every step schedule and both ResizePolicy values: after the converging re-attach (reattach_reflowed_scrollback_or_redetach repeated until None), the unified history (text, wrapped) and its count, display_offset and the screen equal what the same resizes give synchronously by resize_with_policy. Old history precedes window output; a logical line straddling the store/ring seam is rewrapped whole. A width change during the flight detaches nothing; the converging re-attach then re-detaches exactly once at the settled width, carrying ring_out and the staged rows as lazy_lines and leaving the store untouched, and the final store is wrapped at that width.	5	job ownership round trip, ring/lazy/store three-tier composition, loops	0,1,2,3,4,5,7,10,12
offload-reader-position	refinement	Grid::{resize_offloading_scrollback,resize_offloading_scrollback_with_policy,reattach_reflowed_scrollback,reattach_reflowed_scrollback_or_redetach,abort_reflow_offload,reader_aim_held,scroll_display,scroll_to_top,scroll_to_bottom}; GridStorage::reader_live_bottom_gen	Per window (the converging redetach opens its own, with its own baseline): p = display_offset at detach; aim = a held request accumulated by scroll_display deltas that cannot move the view because nothing is attached above it (up adds, down spends the aim first, scroll_to_top aims at the top of the whole history), shifted up by every window line that later scrolls into history; descended = a reader primitive moved the viewport from >0 to 0 during the window (reader-live-bottom-gesture). The detach and redetach keep display_offset ≤ scrollback_lines() at every point; a rows-only resize in the window offloads nothing (returns None) and a counter rise from appended rows is not counted as lines scrolled into history. On reattach: descended, or aimed up then fully back down ⇒ 0. Otherwise restore = p when the rewrap renumbered nothing (the same line stays on top), else min(p,sb'); with an aim the result is max(restore, aim position) (an aim below the restore never pulls the reader down; top = sb' - remaining down-aim). Afterwards reader_aim_held()=false. Outside a window scroll_display is a plain scroll. abort drops the aim and re-clamps after discarding staged output.	17	small state machine (p, aim: Option<i64>, gen baseline, window open/closed) through &mut self; ideal as a ty_model! with a conformance binding; i32/usize clamps	0,1,2,3,4,5,10
reader-live-bottom-gesture	contract	GridStorage::reader_live_bottom_gen; Grid::{scroll_to_bottom,scroll_display,pin_viewport_to_live_for_output_batch,repin_display_offset,resize}	reader_live_bottom_gen increases by exactly 1 when a reader primitive (scroll_to_bottom, or scroll_display with a negative delta) changes display_offset from >0 to 0, and is unchanged by every other op: a reader op that moves nothing or moves up, pin_viewport_to_live_for_output_batch, repin_display_offset (the per-batch force-to-live and re-pin), and any resize re-anchoring the viewport.	1	u64 counter, two-state relation on display_offset	0,2,3,4,6
zero-share-tiered-is-ring-only	equivalence	Grid::{with_tiered_scrollback,set_scrollback_line_limit,resize,resize_no_reflow,resize_offloading_scrollback,reattach_reflowed_scrollback,reattach_reflowed_scrollback_or_redetach,write_char,line_feed,carriage_return}	∀ rows, cols, ring cap C, limit L ≤ C, every script of steps {resize, resize_no_reflow, offloaded resize (detach then reattach[_or_redetach], including a second resize_offloading_scrollback inside the window that returns None and resizes in place), write a line}: t = with_tiered_scrollback(rows,cols,C,store)+set_scrollback_line_limit(Some(L)) and its ring-only twin w = with_scrollback(rows,cols,L) (plain resize for every resize form) satisfy Tr(t)=Tr(w) after every step; the redetach loop ends within 2 passes once the width has settled; t.assert_invariants() holds throughout.	3	whole Grid state machine (ring, lazy buffer, tiered store, reflow job); a bisimulation over two storage representations	0,1,2,3,4,5,7
history-contiguous-suffix	invariant	Grid::{resize,resize_offloading_scrollback,reattach_reflowed_scrollback,erase_scrollback,attach_scrollback,set_scrollback_line_limit,scrollback_lines}	∀ reachable tiered grid whose store share is zero (L ≤ ring cap, including a replacement store attached during an offload window), after any sequence of numbered-line writes and resizes: Tr(g) = [k..=newest] for some k (no hole, no newer line ahead of an older one), scrollback_lines() ≤ L, and once ≥ L lines have scrolled past the screen scrollback_lines() = L; re-attaching a stale worker job after a reset never breaks this.	2	as zero-share-tiered-is-ring-only; an invariant over the composed resize/offload/reattach paths	0,1,2,3,4,5,7
```

### Distribution by kind (206)

| kind | count | ids |
|---|---|---|
| contract | 147 | packed-color-encoding, packed-colors-side-algebra, convert-colors, cell-constructors, cell-read-accessors, cell-mutators, cell-color-accessors, color-new-to-rgb, style-table-new-holds-only-default, current-style-new-fields, current-style-reset, display-offset-damage, damage-row-iterators, damage-bounds-iteration, selection-damage-union-lattice, selection-damage-overflow-absorb, cell-extra-fields, extras-map-basic, extras-clear-ops, extras-shifts, extras-uniform-run, hyperlink-limit, extras-capacity-policy, alloc-slice-contract, page-pool-reuse-and-memory, recycled-row-cells, row-new, orphan-fixup-primitives, row-set, row-narrow-write, row-wide-write, fill-cell-run, cells-mut-with-fixup, ich-insert-chars-fill, dch-delete-chars-fill, ech-erase-chars-with, bounded-ich-dch, clear-range-with-orphan, selective-erase-decsca, bit10-locality, fixup-wide-boundary, row-resize, row-clear-erase-reset, row-line-size, row-copy-from, row-wrapped-flag, row-display, tab-stop-set-clear, ht-next-stop, cbt-prev-stop, tab-stops-resize-grow-only, grid-construct, row-access-bounds, column-geometry, row-u16-saturating, ring-row-shift-permutation, region-and-margin-setters, cursor-set-clamp, cursor-vertical-margins, cursor-horizontal-margins, carriage-return, line-feed, reverse-line-feed, autowrap-deferred, writes-replace-extras, wide-autowrap-tail-bce, grid-cell-text-reads, row-metadata-accessors, buffer-access-lines, viewport-memo-sharing, scroll-region-shift, top-anchored-region-archive, el-span, erase-bce-orphan, ed-span, ed-line-attributes, right-erase-breaks-wrap-link, selective-erase-span, selective-erase-pending-wrap, ed3-erase-scrollback, decaln, rect-fill-erase, decsera, deccara-decrara, reverse-cell-attrs, deccra-copy, ich-dch-shift, ech-erase-chars, il-dl, il-dl-margined, decic-decdc, unscroll-denotation, row-band-move-compose, row-band-moves-denote-ops, viewport-scroll-damage, display-offset-navigation, content-damage-and-generation, mark-cursor-damage, continue-absolute-numbering, history-line-order, history-read-errors, history-fence-export, history-fence-breaks, reserve-older-history-keys, attach-older-history, older-history-build, retention-limit, ring-watermark-level, grid-memory-accounting, lazy-buffer-pool-and-sums, lazy-spine-slack-bound, compress-offload-staging, flood-backlog-cap, trickle-skip-rule, flood-marker-line, extract-row-extras-gate, extract-row-extras-spans, row-to-line-colour-resolution, scrolled-row-extras-is-empty, materialize-from-line-denotation, fill-row-from-line, checkpoint-lines-layout, reflow-mode-from-bool, resize-clamp-and-reset, reflow-cursor-tracking, reflow-viewport-placement, width-change-seam-policy, conpty-width-demote, double-size-rows-resize-in-place, reflow-extras-follow-cells, remap-copied-extra, adjust-chunk-boundary, cursor-in-chunk, source-coords-for-row, copy-cells-to-row, chunk-cells-to-rows, reflow-history-rewrap, scrollback-rewrap-denotation, history-renumber-epoch-on-width-reflow, no-reflow-resize-truncates, resize-keeps-reader-position, rows-grow-policy, rows-shrink-shape, resize-undo-validity, truecolor-survives-resize, offload-window-cap-logging, reader-live-bottom-gesture |
| refinement | 15 | damage-tracker-model, ring-model, restore-tab-stops-admission, write-char-denotation, scroll-up-appends-transcript, top-anchored-scroll-refinement, unified-retention-limit-refinement, grapheme-unit-segmentation, reflow-rewrap-layout, rows-only-resize-transcript, resize-row-reuse-refines-model, offload-window-refinement, offload-window-conserves-history, offload-refines-plain-resize, offload-reader-position |
| equivalence | 13 | cell-flags-bit-algebra, extras-render-data, row-get-unchecked, run-writers-refine-write-char-wrap, viewport-memo-transparent, history-walk-refines-get, deferred-line-equals-eager, line-consumers-agree, stored-cells-run-expansion, grapheme-ascii-fast-path, resize-undo-flap-identity, offload-step-schedule-independence, zero-share-tiered-is-ring-only |
| invariant | 13 | current-style-cache-invariant, image-layer-stack, hyperlink-bound-invariant, page-store-row-separation, early-wrap-hole-lifecycle, grid-type-invariant, cursor-in-bounds, has-extras-iff-entry, content-gen-tracks-content, absolute-row-numbering, history-link-budget, flood-history-no-silent-hole, history-contiguous-suffix |
| cost | 7 | hyperlink-walk-cost, extras-op-cost, rgb-harvest-cost, rows-only-resize-cost, reflow-cost, scroll-off-is-lazy, width-resize-sync-cost |
| table-property | 6 | cell-flags-layout, wide-continuation-of, attrs-to-cell-flags-table, tab-default-stops, pending-wrap-motion-table, erase-pending-wrap-table |
| round-trip | 3 | row-line-roundtrip, checkpoint-visible-roundtrip, width-roundtrip-restores-topology |
| other | 1 | page-send-sync-sound |
| no-panic | 1 | row-unsafe-ub-free |

### Blocking-gap histogram

`gap → specs blocked by it`. No spec is blocked by gap 0 alone: every one also needs at least one
widening (most often 1, non-scalar carriers; 2, certified `&self`/`&mut self`; 3, E6 body widening).

| gap | specs blocked |
|---|---|
| 0 | 206 |
| 1 | 184 |
| 2 | 172 |
| 3 | 175 |
| 4 | 102 |
| 5 | 109 |
| 6 | 59 |
| 7 | 120 |
| 8 | 60 |
| 10 | 12 |
| 11 | 6 |
| 12 | 48 |
| 13 | 18 |
| 14 | 1 |
| 16 | 2 |

### Proof dependency order

1. Scalar and const facts: `cell-flags-layout`, `row-u16-saturating`, `wide-continuation-of`,
   `cell-flags-bit-algebra`, `packed-color-encoding`, `packed-colors-side-algebra`, `color-new-to-rgb`,
   `reverse-cell-attrs`, `cursor-in-chunk`, `adjust-chunk-boundary`, `display-offset-damage`, `reflow-mode-from-bool`,
   `flood-marker-line`, `row-band-move-compose`.
2. Cell values: `convert-colors`, `cell-constructors`, `cell-read-accessors`, `cell-mutators`, `cell-color-accessors`,
   `attrs-to-cell-flags-table`, then `current-style-new-fields`, `current-style-reset`, `current-style-cache-invariant`,
   `style-table-new-holds-only-default`.
3. Small `&mut self` state on the grid: `column-geometry`, `region-and-margin-setters`, `cursor-set-clamp`,
   `cursor-vertical-margins`, `cursor-horizontal-margins`, `carriage-return`, `cursor-in-bounds`,
   `pending-wrap-motion-table`, `tab-default-stops`, `tab-stop-set-clear`, `ht-next-stop`, `cbt-prev-stop`,
   `tab-stops-resize-grow-only`, `continue-absolute-numbering`, `reader-live-bottom-gesture`, `trickle-skip-rule`.
4. Damage: `damage-row-iterators` → `damage-tracker-model` → `damage-bounds-iteration`, `mark-cursor-damage`;
   `selection-damage-union-lattice`, `selection-damage-overflow-absorb`.
5. Extras: `cell-extra-fields`, `image-layer-stack` → `ring-model` → `extras-map-basic` → `extras-clear-ops`,
   `extras-shifts`, `extras-render-data`, `extras-uniform-run` → `hyperlink-bound-invariant` → `hyperlink-limit`;
   `extras-capacity-policy`.
6. Pages and rows (the unsafe lane, gap 8): `alloc-slice-contract`, `page-pool-reuse-and-memory`,
   `recycled-row-cells`, `page-store-row-separation`, `page-send-sync-sound` → `row-new`, `row-get-unchecked`,
   `orphan-fixup-primitives` → `row-set`, `row-narrow-write`, `row-wide-write`, `fill-cell-run`,
   `cells-mut-with-fixup`, `clear-range-with-orphan`, `ech-erase-chars-with`, `ich-insert-chars-fill`,
   `dch-delete-chars-fill`, `bounded-ich-dch`, `selective-erase-decsca`, `fixup-wide-boundary`, `row-resize`,
   `row-line-size`, `row-copy-from`, `row-clear-erase-reset`, `row-wrapped-flag`, `row-display` → the frame
   corollaries `bit10-locality`, `early-wrap-hole-lifecycle` → `row-unsafe-ub-free`.
7. Grid screen ops over the row lemmas: `grid-construct`, `row-access-bounds`, `ring-row-shift-permutation`,
   `write-char-denotation`, `autowrap-deferred`, `run-writers-refine-write-char-wrap`, `writes-replace-extras`,
   `wide-autowrap-tail-bce`, `has-extras-iff-entry`, then `line-feed`, `reverse-line-feed`, `scroll-region-shift`,
   the erase family (`el-span`, `erase-bce-orphan`, `ed-span`, `ed-line-attributes`, `selective-erase-span`,
   `selective-erase-pending-wrap`, `erase-pending-wrap-table`, `right-erase-breaks-wrap-link`, `decaln`,
   `rect-fill-erase`, `decsera`, `deccara-decrara`, `deccra-copy`), line ops (`ich-dch-shift`, `ech-erase-chars`,
   `il-dl`, `il-dl-margined`, `decic-decdc`), `row-band-moves-denote-ops`, `content-damage-and-generation`,
   `viewport-scroll-damage`, `display-offset-navigation`, `grid-cell-text-reads`, `row-metadata-accessors`,
   `grid-type-invariant` (it frames every mutator, so it comes after them).
8. History and lines: `history-line-order`, `history-read-errors`, `history-walk-refines-get`,
   `deferred-line-equals-eager`, `lazy-buffer-pool-and-sums`, `extract-row-extras-*`, `row-to-line-colour-resolution`,
   `materialize-from-line-denotation`, `grapheme-unit-segmentation`, `grapheme-ascii-fast-path`,
   `stored-cells-run-expansion` → `row-line-roundtrip`, `line-consumers-agree`, `fill-row-from-line` →
   `scroll-up-appends-transcript`, `top-anchored-region-archive`, `unscroll-denotation`, `buffer-access-lines`,
   `viewport-memo-*`, `checkpoint-*`, `retention-limit`, `ed3-erase-scrollback`, `absolute-row-numbering`,
   `history-fence-*`, `reserve-older-history-keys`, `older-history-build`, `attach-older-history`,
   flood/lazy specs, `history-link-budget`, `grid-memory-accounting`, `ring-watermark-level`.
9. Resize and reflow: `copy-cells-to-row`, `remap-copied-extra`, `source-coords-for-row` → `chunk-cells-to-rows`
   → `reflow-rewrap-layout`, `reflow-cursor-tracking`, `reflow-extras-follow-cells`, `double-size-rows-resize-in-place`,
   `scrollback-rewrap-denotation` → `reflow-history-rewrap`, `reflow-viewport-placement`, `width-change-seam-policy`,
   `conpty-width-demote`, `no-reflow-resize-truncates`, `rows-only-resize-transcript`, `rows-grow-policy`,
   `rows-shrink-shape`, `resize-undo-*`, `truecolor-survives-resize`, `resize-clamp-and-reset`,
   `resize-keeps-reader-position`, `history-renumber-epoch-on-width-reflow` → `width-roundtrip-restores-topology`.
10. Offload window and model refinements (gaps 10 and 13): `offload-step-schedule-independence`,
    `offload-window-conserves-history`, `offload-window-cap-logging`, `offload-reader-position`,
    `offload-refines-plain-resize`, `offload-window-refinement`, `unified-retention-limit-refinement`,
    `top-anchored-scroll-refinement`, `resize-row-reuse-refines-model`, `restore-tab-stops-admission`,
    `zero-share-tiered-is-ring-only`, `history-contiguous-suffix`.
11. Cost specs last (they need ghost state): `hyperlink-walk-cost`, `extras-op-cost`, `rgb-harvest-cost`,
    `rows-only-resize-cost`, `reflow-cost`, `scroll-off-is-lazy`, `width-resize-sync-cost`.

## 2. Test → spec map (TSV; all 1805)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples. Tests
that sample no spec carry `nonspec:<action>` (§4).

```tsv
file	test	primary	also
crates/aterm-grid/src/cell_colors.rs	packed_color_every_mode	packed-color-encoding	
crates/aterm-grid/src/cell_colors.rs	packed_color_indexed_raw_encoding	packed-color-encoding	
crates/aterm-grid/src/cell_colors.rs	packed_color_rgb_raw_encoding	packed-color-encoding	
crates/aterm-grid/src/cell_colors.rs	packed_colors_every_fg_bg_and_extras_state	packed-colors-side-algebra	
crates/aterm-grid/src/cell_constructors.rs	test_convert_colors_default_default	convert-colors	
crates/aterm-grid/src/cell_constructors.rs	test_convert_colors_indexed_bg	convert-colors	
crates/aterm-grid/src/cell_constructors.rs	test_convert_colors_indexed_fg	convert-colors	
crates/aterm-grid/src/cell_constructors.rs	test_convert_colors_rgb_bg_marks_rgb_mode	convert-colors	
crates/aterm-grid/src/cell_constructors.rs	test_convert_colors_rgb_fg_marks_rgb_mode	convert-colors	
crates/aterm-grid/src/cell_constructors.rs	test_from_ascii_fast_matches_new_for_ascii	cell-constructors	
crates/aterm-grid/src/cell_constructors.rs	test_from_ascii_with_style_id_merges_flags	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_from_ascii_with_style_id_sets_style	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_from_ascii_with_style_id_stores_byte	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_from_raw_parts_preserves_complex_cell	cell-constructors	
crates/aterm-grid/src/cell_constructors.rs	test_with_overflow_index_default_colors	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_with_overflow_index_max_value	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_with_overflow_index_returns_replacement_char	cell-read-accessors	
crates/aterm-grid/src/cell_constructors.rs	test_with_overflow_index_sets_complex_flag	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_with_overflow_index_stores_index	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_with_style_id_max_style_id	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_with_style_id_merges_cell_flags	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_with_style_id_non_bmp_stores_replacement	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_with_style_id_preserves_character	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_with_style_id_sets_uses_style_id_flag	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	test_with_style_id_stores_style_id_in_colors	nonspec:delete	
crates/aterm-grid/src/cell_constructors.rs	value_constructors_store_exactly_what_they_are_given	cell-constructors	
crates/aterm-grid/src/cell_flags.rs	bit_layout_is_pinned	cell-flags-layout	
crates/aterm-grid/src/cell_flags.rs	bitand_assign_operator	cell-flags-bit-algebra	
crates/aterm-grid/src/cell_flags.rs	bitand_disjoint_is_empty	cell-flags-bit-algebra	
crates/aterm-grid/src/cell_flags.rs	bitand_operator	cell-flags-bit-algebra	
crates/aterm-grid/src/cell_flags.rs	bitor_assign_operator	cell-flags-bit-algebra	
crates/aterm-grid/src/cell_flags.rs	bitor_operator	cell-flags-bit-algebra	
crates/aterm-grid/src/cell_flags.rs	contains_combo_flag	cell-flags-bit-algebra	
crates/aterm-grid/src/cell_flags.rs	contains_requires_all_bits	cell-flags-bit-algebra	
crates/aterm-grid/src/cell_flags.rs	methods_agree_with_raw_bit_arithmetic	cell-flags-bit-algebra	
crates/aterm-grid/src/cell_flags.rs	not_operator	cell-flags-bit-algebra	
crates/aterm-grid/src/cell_tests.rs	cell_char_data_at_byte_offset_zero	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_cjk	cell-constructors	
crates/aterm-grid/src/cell_tests.rs	cell_clear	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_clear_resets_has_extras	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_clear_style_id	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_colors_at_byte_offset_two	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_flags_at_byte_offset_six	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_flags_uses_style_id	cell-flags-bit-algebra	
crates/aterm-grid/src/cell_tests.rs	cell_from_ascii_styled_with_extras_flag	cell-constructors	
crates/aterm-grid/src/cell_tests.rs	cell_from_ascii_with_style_id	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_has_extras_default_false	cell-read-accessors	
crates/aterm-grid/src/cell_tests.rs	cell_has_extras_preserved_with_colors	cell-mutators	
crates/aterm-grid/src/cell_tests.rs	cell_inline_color_accessors_return_inline_colors	cell-color-accessors	
crates/aterm-grid/src/cell_tests.rs	cell_is_empty	cell-read-accessors	
crates/aterm-grid/src/cell_tests.rs	cell_new_bmp	cell-constructors	
crates/aterm-grid/src/cell_tests.rs	cell_new_non_bmp	cell-constructors	
crates/aterm-grid/src/cell_tests.rs	cell_pack_unpack_flags	cell-constructors	
crates/aterm-grid/src/cell_tests.rs	cell_packing_is_lossless_over_every_input	cell-constructors	
crates/aterm-grid/src/cell_tests.rs	cell_rgb_needs_overflow	cell-color-accessors	
crates/aterm-grid/src/cell_tests.rs	cell_set_has_extras_roundtrip	cell-mutators	
crates/aterm-grid/src/cell_tests.rs	cell_set_methods	cell-mutators	
crates/aterm-grid/src/cell_tests.rs	cell_set_style_id	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_style_id_color_accessors_return_none	cell-color-accessors	
crates/aterm-grid/src/cell_tests.rs	cell_style_id_max_value	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_style_id_opt_when_using_inline_colors	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_style_id_opt_when_using_style	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_with_overflow_index	cell-read-accessors	
crates/aterm-grid/src/cell_tests.rs	cell_with_style_id_default	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_with_style_id_non_default	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_with_style_id_preserves_cell_flags	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	cell_with_style_id_wide_continuation	nonspec:delete	
crates/aterm-grid/src/cell_tests.rs	has_extras_is_independent_of_complex_colours_and_style_ids	packed-colors-side-algebra	
crates/aterm-grid/src/cell_tests.rs	new_non_bmp_stores_replacement	cell-constructors	
crates/aterm-grid/src/cell_tests.rs	set_char_bmp_stores_codepoint	cell-mutators	
crates/aterm-grid/src/cell_tests.rs	set_char_clears_complex_flag	cell-mutators	
crates/aterm-grid/src/cell_tests.rs	set_char_constructor_parity	cell-mutators	
crates/aterm-grid/src/damage/iter.rs	bitset_iter_all_bits_set_in_word	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	bitset_iter_cross_word_boundary	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	bitset_iter_empty_bits_yields_nothing	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	bitset_iter_empty_slice_yields_nothing	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	bitset_iter_max_row_truncates_results	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	bitset_iter_max_row_zero_yields_nothing	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	bitset_iter_single_bit_last_in_word	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	bitset_iter_single_bit_zero	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	bitset_iter_size_hint_decreases_as_consumed	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	bitset_iter_size_hint_upper_bound	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	bounds_iter_empty_damage_yields_nothing	damage-bounds-iteration	
crates/aterm-grid/src/damage/iter.rs	bounds_iter_full_damage_covers_all_cols	damage-bounds-iteration	
crates/aterm-grid/src/damage/iter.rs	bounds_iter_single_cell_damage	damage-bounds-iteration	
crates/aterm-grid/src/damage/iter.rs	full_iter_empty_range	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	full_iter_size_hint_exact	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	full_iter_yields_all_rows	damage-row-iterators	
crates/aterm-grid/src/damage/iter.rs	partial_iter_delegates_to_bitset	damage-row-iterators	
crates/aterm-grid/src/damage/property_tests.rs	damage_bounds_iterator_empty_damage_returns_no_rows	damage-bounds-iteration	
crates/aterm-grid/src/damage/property_tests.rs	damage_bounds_iterator_single_row_preserves_column_bounds	damage-bounds-iteration	
crates/aterm-grid/src/damage/property_tests.rs	mark_random_cells_tracks_expected_rows_and_bounds	damage-tracker-model	
crates/aterm-grid/src/damage/property_tests.rs	merged_damage_iterator_full_damage_yields_single_rect	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	bitset_and_bounds_iterators_are_exact	damage-row-iterators	
crates/aterm-grid/src/damage/tests.rs	bitset_iterator_multi_word	damage-row-iterators	
crates/aterm-grid/src/damage/tests.rs	bitset_iterator_respects_max	damage-row-iterators	
crates/aterm-grid/src/damage/tests.rs	bitset_iterator_single_word	damage-row-iterators	
crates/aterm-grid/src/damage/tests.rs	damage_bounds_iterator_skips_multiple_out_of_range	damage-bounds-iteration	
crates/aterm-grid/src/damage/tests.rs	damage_bounds_iterator_skips_out_of_range_row	damage-bounds-iteration	
crates/aterm-grid/src/damage/tests.rs	damage_full	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	damage_has_damage	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	damage_iter_bounds	damage-bounds-iteration	
crates/aterm-grid/src/damage/tests.rs	damage_iter_merged_consecutive_overlapping	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	damage_iter_merged_full_rows	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	damage_iter_merged_gap	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	damage_iter_merged_non_overlapping	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	damage_iterator	damage-bounds-iteration	
crates/aterm-grid/src/damage/tests.rs	damage_iterator_empty	damage-bounds-iteration	
crates/aterm-grid/src/damage/tests.rs	damage_iterator_full	damage-bounds-iteration	
crates/aterm-grid/src/damage/tests.rs	damage_iterator_size_hint_after_exhaustion	damage-row-iterators	
crates/aterm-grid/src/damage/tests.rs	damage_iterator_sparse	damage-bounds-iteration	
crates/aterm-grid/src/damage/tests.rs	damage_partial_cell	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	damage_partial_row	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	damage_rect_dimensions	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	damage_rect_dimensions_consistent	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	damage_rect_extend	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	damage_rect_from_line	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	damage_reset	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	damage_reset_from_full_creates_fresh	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	damage_reset_repeated_cycles	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	damage_reset_reuses_allocation_partial_to_partial	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	damage_row_bounds_clamped_to_cols	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	damaged_row_count	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	line_damage_bounds_is_empty	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	line_damage_bounds_merge	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	line_damage_bounds_no_merge_gap	nonspec:delete	
crates/aterm-grid/src/damage/tests.rs	mark_rows_empty_range	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	mark_rows_range_behavior	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	row_damage_bounds_merge_near_u16_max_boundary	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	row_damage_bounds_near_u16_max_boundary	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	row_damage_bounds_partial_clamp	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	row_damage_bounds_small_cols_clamps_to_none	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_full_all_rows_damaged	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_full_operations_idempotent	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_full_row_bounds_full_width	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_machine_cycle	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_mark_cell_preserves_partial	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_mark_full_transitions	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_mark_row_preserves_partial	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_mark_rows_preserves_partial	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_new_creates_partial	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_partial_marked_is_damaged	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	state_reset_transitions_full_to_partial	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	tracker_clear_equivalent_to_new_same_size	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	tracker_clear_resizes_on_row_count_change	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	tracker_many_rows	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	tracker_mark_cell_u16_max_saturates_right_bound	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	tracker_out_of_bounds_is_row_damaged_returns_false	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	tracker_out_of_bounds_mark_cell_is_safe	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	tracker_out_of_bounds_mark_row_is_safe	damage-tracker-model	
crates/aterm-grid/src/damage/tests.rs	tracker_out_of_bounds_row_damage_bounds_returns_none	damage-tracker-model	
crates/aterm-grid/src/damage/tests_display_offset.rs	compute_display_offset_damage_rows	display-offset-damage	
crates/aterm-grid/src/damage/tests_display_offset.rs	display_offset_damage_classifies_every_delta_and_size	display-offset-damage	
crates/aterm-grid/src/damage/tests_display_offset.rs	test_apply_bottom_rows_marks_correct_rows	display-offset-damage	
crates/aterm-grid/src/damage/tests_display_offset.rs	test_apply_full_marks_full	display-offset-damage	
crates/aterm-grid/src/damage/tests_display_offset.rs	test_apply_none_does_nothing	display-offset-damage	
crates/aterm-grid/src/damage/tests_display_offset.rs	test_apply_top_rows_marks_correct_rows	display-offset-damage	
crates/aterm-grid/src/extra_collection.rs	clear_range_matches_per_cell_clear	ring-model	
crates/aterm-grid/src/extra_collection.rs	complex_clear_range_matches_per_cell_clear	ring-model	
crates/aterm-grid/src/extra_collection.rs	fill_run_matches_per_cell_set	ring-model	
crates/aterm-grid/src/extra_collection.rs	per_cell_hyperlink_edit_does_not_leak_across_the_run	extras-uniform-run	
crates/aterm-grid/src/extra_collection.rs	test_cell_extras_clear_range_clears_rgb_ring	extras-clear-ops	
crates/aterm-grid/src/extra_collection.rs	test_cell_extras_clear_rect_clears_rgb_ring	extras-clear-ops	
crates/aterm-grid/src/extra_collection.rs	test_cell_extras_clear_row_clears_rgb_ring	extras-clear-ops	
crates/aterm-grid/src/extra_collection.rs	test_cell_extras_clear_rows_clears_rgb_ring	extras-clear-ops	
crates/aterm-grid/src/extra_collection.rs	test_complex_ring_get_col_at_stride_returns_none	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_complex_ring_overscroll_resets_offset	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_complex_ring_scroll_exactly_visible_rows	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_complex_ring_scroll_then_oob_col_returns_none	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_complex_ring_set_col_at_stride_is_noop	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_complex_ring_shift_region_up_preserves_via_memmove	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_render_data_for_cell_prefers_ring_values	extras-render-data	
crates/aterm-grid/src/extra_collection.rs	test_render_data_for_cell_reads_hashmap_fallbacks	extras-render-data	
crates/aterm-grid/src/extra_collection.rs	test_rgb_ring_clear_range_zeroes_entries	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_rgb_ring_clear_row_zeroes_entire_row	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_rgb_ring_get_col_at_stride_returns_none	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_rgb_ring_overscroll_resets_offset	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_rgb_ring_set_col_at_stride_is_noop	ring-model	
crates/aterm-grid/src/extra_collection.rs	test_rgb_ring_shift_region_up_preserves_via_memmove	ring-model	
crates/aterm-grid/src/extra_collection_shifts.rs	test_cols_left_shifts_and_preserves_ring_bg	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_cols_right_shifts_and_preserves_ring_bg	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_declrmm_rect_scroll_down	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_declrmm_rect_scroll_up	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_decstbm_scroll_down_within_margins	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_decstbm_scroll_up_within_margins	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_rect_up_only_affects_rect_ring_bg	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_region_down_shifts_and_preserves_ring_bg	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_region_up_preserves_out_of_region_ring_bg	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_region_up_shifts_in_region_ring_bg	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_cols_left_delete_everything_on_row	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_cols_left_deletes_range	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_cols_left_preserves_beyond_max_col	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_cols_left_preserves_cols_before_start	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_cols_right_at_max_col_boundary	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_cols_right_drops_past_max_col	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_cols_right_preserves_beyond_max_col	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_cols_right_preserves_cols_before_start	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_cols_right_preserves_extra_data	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_rect_down_drops_bottom_rows_in_rect	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_rect_down_only_affects_column_range	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_rect_down_zero_n_is_noop	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_rect_up_deletes_top_rows_in_rect	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_rect_up_only_affects_column_range	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_rect_up_preserves_outside_row_range	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_rect_up_zero_n_is_noop	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_region_down_drops_bottom_rows	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_region_down_n_equals_region_size	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_region_down_single_row_shift	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_region_out_of_region_extras_skip_drain_rebuild	nonspec:move-to-bench	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_region_up_boundary_at_top_equals_n	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_region_up_full_screen	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_region_up_multiple_cols_same_row	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_region_up_preserves_extra_data	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_region_up_single_entry_at_boundary	extras-shifts	
crates/aterm-grid/src/extra_collection_shifts.rs	test_shift_region_up_with_accumulated_offset	extras-shifts	
crates/aterm-grid/src/extra_tests.rs	cell_extra_default_is_empty	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	cell_extra_extended_flags	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	cell_extra_max_combining	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	cell_extra_packed_rgb_colors	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	cell_extra_size_optimized	nonspec:delete	
crates/aterm-grid/src/extra_tests.rs	cell_extra_stacks_image_layers_in_draw_order	image-layer-stack	
crates/aterm-grid/src/extra_tests.rs	cell_extra_with_combining	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	cell_extra_with_hyperlink	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	cell_extra_with_image	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	cell_extra_with_underline_color	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	cell_extras_clear_range_real	extras-clear-ops	
crates/aterm-grid/src/extra_tests.rs	cell_extras_clear_rect_batch	extras-clear-ops	
crates/aterm-grid/src/extra_tests.rs	cell_extras_clear_rect_empty_cols	extras-clear-ops	
crates/aterm-grid/src/extra_tests.rs	cell_extras_clear_rect_real	extras-clear-ops	
crates/aterm-grid/src/extra_tests.rs	cell_extras_clear_releases_capacity	extras-capacity-policy	
crates/aterm-grid/src/extra_tests.rs	cell_extras_clear_row	extras-clear-ops	
crates/aterm-grid/src/extra_tests.rs	cell_extras_clear_rows_batch	extras-clear-ops	
crates/aterm-grid/src/extra_tests.rs	cell_extras_clear_rows_empty_range	extras-clear-ops	
crates/aterm-grid/src/extra_tests.rs	cell_extras_clear_rows_shrinks_when_excess	extras-capacity-policy	
crates/aterm-grid/src/extra_tests.rs	cell_extras_empty_removed	extras-map-basic	
crates/aterm-grid/src/extra_tests.rs	cell_extras_retain_no_shrink_on_small_map	extras-capacity-policy	
crates/aterm-grid/src/extra_tests.rs	cell_extras_shift_cols_left	extras-shifts	
crates/aterm-grid/src/extra_tests.rs	cell_extras_shift_cols_right	extras-shifts	
crates/aterm-grid/src/extra_tests.rs	cell_extras_shift_cols_zero_count_is_noop	extras-shifts	
crates/aterm-grid/src/extra_tests.rs	cell_extras_shift_rows_up	extras-shifts	
crates/aterm-grid/src/extra_tests.rs	cell_extras_storage	extras-map-basic	
crates/aterm-grid/src/extra_tests.rs	complex_char_roundtrip	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	hyperlink_guard_counts_links_not_total_entries	hyperlink-walk-cost	
crates/aterm-grid/src/extra_tests.rs	hyperlink_has_data_consistent	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	hyperlink_upper_bound_never_under_counts	hyperlink-bound-invariant	
crates/aterm-grid/src/extra_tests.rs	is_combining_mark_all_ranges	nonspec:delete	
crates/aterm-grid/src/extra_tests.rs	is_combining_mark_basic	nonspec:delete	
crates/aterm-grid/src/extra_tests.rs	row_has_hyperlinks_after_clear_range	extras-clear-ops	
crates/aterm-grid/src/extra_tests.rs	row_has_hyperlinks_detection	nonspec:delete	
crates/aterm-grid/src/extra_tests.rs	row_has_hyperlinks_partial_removal	nonspec:delete	
crates/aterm-grid/src/extra_tests.rs	set_hyperlink_clears_stale_id_on_url_replacement	cell-extra-fields	
crates/aterm-grid/src/extra_tests.rs	shift_cols_left_preserves_non_target_rows	extras-shifts	
crates/aterm-grid/src/extra_tests.rs	shift_cols_left_respects_max_col_boundary	extras-shifts	
crates/aterm-grid/src/extra_tests.rs	shift_cols_right_preserves_non_target_rows	extras-shifts	
crates/aterm-grid/src/extra_tests.rs	small_map_never_walks_however_inflated_the_counter	hyperlink-walk-cost	
crates/aterm-grid/src/extra_tests.rs	test_hyperlink_limit_evicts_oldest_rows_first	hyperlink-limit	
crates/aterm-grid/src/extra_tests.rs	test_hyperlink_limit_evicts_oldest_when_exceeded	hyperlink-limit	
crates/aterm-grid/src/extra_tests.rs	test_hyperlink_limit_large_map_few_hyperlinks_no_eviction	hyperlink-limit	
crates/aterm-grid/src/extra_tests.rs	test_hyperlink_limit_normal_usage_under_limit	hyperlink-limit	
crates/aterm-grid/src/extra_tests.rs	test_hyperlink_limit_preserves_non_hyperlink_extras	hyperlink-limit	
crates/aterm-grid/src/extra_tests.rs	underline_color_u32_roundtrip	cell-extra-fields	
crates/aterm-grid/src/extra_tests/overflow.rs	shift_region_up_by_u16_overflow_drops_region	extras-shifts	
crates/aterm-grid/src/extra_tests/overflow.rs	shift_rows_up_by_exact_u16_max_boundary	extras-shifts	
crates/aterm-grid/src/extra_tests/overflow.rs	shift_rows_up_by_u16_overflow_drops_all	extras-shifts	
crates/aterm-grid/src/extra_tests/scaling_proofs.rs	clear_row_cost_linear_in_total_extras	extras-op-cost	
crates/aterm-grid/src/extra_tests/scaling_proofs.rs	compaction_at_threshold_costs_o_e	extras-op-cost	
crates/aterm-grid/src/extra_tests/scaling_proofs.rs	scroll_compound_cost_amortized_o1	extras-op-cost	
crates/aterm-grid/src/extra_tests/scaling_proofs.rs	shift_rows_up_by_full_screen_is_o1	extras-op-cost	
crates/aterm-grid/src/extra_tests/scaling_proofs.rs	shift_rows_up_by_partial_is_o_e	extras-op-cost	
crates/aterm-grid/src/extra_tests/shift_region_tests.rs	cell_extras_shift_region_down_by_basic	extras-shifts	
crates/aterm-grid/src/extra_tests/shift_region_tests.rs	cell_extras_shift_region_down_by_zero_is_noop	extras-shifts	
crates/aterm-grid/src/extra_tests/shift_region_tests.rs	cell_extras_shift_region_up_by_basic	extras-shifts	
crates/aterm-grid/src/extra_tests/shift_region_tests.rs	cell_extras_shift_region_up_by_deletes_top_row_explicitly	extras-shifts	
crates/aterm-grid/src/extra_tests/shift_region_tests.rs	cell_extras_shift_region_up_by_zero_is_noop	extras-shifts	
crates/aterm-grid/src/extra_tests/shift_region_tests.rs	shift_region_down_n_exceeds_region_drops_all_in_region	extras-shifts	
crates/aterm-grid/src/extra_tests/shift_region_tests.rs	shift_region_up_n_equals_region_size_deletes_all	extras-shifts	
crates/aterm-grid/src/extra_tests/shift_region_tests.rs	shift_region_up_single_row_region_deletes_only_top	extras-shifts	
crates/aterm-grid/src/grid/buffer_access.rs	every_read_answers_the_live_screen_at_any_display_offset	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_char_at_negative_line_no_scrollback	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_char_at_out_of_bounds_row	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_char_at_reads_scrollback	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_char_at_resolves_supplementary_plane	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_is_wide_reads_scrollback	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_line_len	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_line_len_reads_scrollback	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_line_text	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_line_text_includes_supplementary_plane	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_line_text_reads_scrollback	buffer-access-lines	
crates/aterm-grid/src/grid/buffer_access.rs	test_resolved_char_bmp_unchanged	nonspec:delete	
crates/aterm-grid/src/grid/buffer_access.rs	test_resolved_char_supplementary_plane_emoji	nonspec:delete	
crates/aterm-grid/src/grid/buffer_access.rs	test_row_text_includes_supplementary_plane	nonspec:delete	
crates/aterm-grid/src/grid/buffer_access.rs	test_scrollback_out_of_range	buffer-access-lines	
crates/aterm-grid/src/grid/checkpoint_lines.rs	roundtrip_combining_mark	checkpoint-visible-roundtrip	
crates/aterm-grid/src/grid/checkpoint_lines.rs	roundtrip_complex_char	checkpoint-visible-roundtrip	
crates/aterm-grid/src/grid/checkpoint_lines.rs	roundtrip_hyperlink	checkpoint-visible-roundtrip	
crates/aterm-grid/src/grid/checkpoint_lines.rs	roundtrip_plain_text	checkpoint-visible-roundtrip	
crates/aterm-grid/src/grid/checkpoint_lines.rs	roundtrip_scrollback_fills	checkpoint-lines-layout	
crates/aterm-grid/src/grid/checkpoint_lines.rs	roundtrip_sgr_colors_and_attrs	checkpoint-visible-roundtrip	
crates/aterm-grid/src/grid/checkpoint_lines.rs	roundtrip_wide_char	checkpoint-visible-roundtrip	
crates/aterm-grid/src/grid/checkpoint_lines.rs	roundtrip_wrapped_line	checkpoint-visible-roundtrip	
crates/aterm-grid/src/grid/checkpoint_lines.rs	visible_checkpoint_captures_live_rows_while_viewport_is_scrolled_up	checkpoint-lines-layout	
crates/aterm-grid/src/grid/checkpoint_lines.rs	visible_checkpoint_cost_does_not_scale_with_scrollback	checkpoint-lines-layout	
crates/aterm-grid/src/grid/construct.rs	test_new_clamps_both_zero_to_one	grid-construct	
crates/aterm-grid/src/grid/construct.rs	test_new_clamps_oversize_to_max	grid-construct	
crates/aterm-grid/src/grid/construct.rs	test_new_clamps_zero_cols_to_one	grid-construct	
crates/aterm-grid/src/grid/construct.rs	test_new_clamps_zero_rows_to_one	grid-construct	
crates/aterm-grid/src/grid/construct.rs	test_new_invariants_hold	grid-construct	
crates/aterm-grid/src/grid/construct.rs	test_with_scrollback_invariants_hold	grid-construct	
crates/aterm-grid/src/grid/construct.rs	test_with_tiered_scrollback_clamps_zero	grid-construct	
crates/aterm-grid/src/grid/construct.rs	test_with_tiered_scrollback_invariants_hold	grid-construct	
crates/aterm-grid/src/grid/content_queries.rs	memory_used_counts_blank_resize_undo_capacity	grid-memory-accounting	
crates/aterm-grid/src/grid/content_queries.rs	memory_used_counts_resize_undo_cells_and_retained_extras	grid-memory-accounting	
crates/aterm-grid/src/grid/content_queries.rs	test_memory_used_larger_grid_uses_more	grid-memory-accounting	
crates/aterm-grid/src/grid/content_queries.rs	test_memory_used_positive	grid-memory-accounting	
crates/aterm-grid/src/grid/content_queries.rs	test_resolved_char_after_overwrite	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_resolved_char_ascii	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_resolved_char_bmp_cjk	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_resolved_char_empty_cell	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_resolved_char_multiple_positions	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_resolved_char_out_of_bounds_col	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_resolved_char_out_of_bounds_row	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_resolved_char_supplementary_plane	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_cols_into_column_indexed_wide_and_fill	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_after_overwrite	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_ascii	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_complex_emoji_via_extras	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_complex_with_combining	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_empty_row	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_full_row	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_into_matches_row_text_and_reuses_capacity	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_last_visible_row	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_minimum_grid	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_mixed_wide_and_narrow	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_multiple_rows	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_null_cells_become_spaces	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_out_of_bounds	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_supplementary_plane_emoji	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_row_text_wide_char_continuation_skipped	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_visible_content_line_count	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_visible_content_preserves_row_order	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_visible_content_single_row_grid	grid-cell-text-reads	
crates/aterm-grid/src/grid/content_queries.rs	test_visible_content_with_wide_chars	grid-cell-text-reads	
crates/aterm-grid/src/grid/cursor_ops.rs	accessors_after_sequential_moves	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	accessors_consistent_with_cursor_struct	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	backspace_at_col_zero_stays	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	backspace_clears_pending_wrap	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	carriage_return_clears_pending_wrap	carriage-return	
crates/aterm-grid/src/grid/cursor_ops.rs	carriage_return_margin_mode_cursor_in_margins	carriage-return	
crates/aterm-grid/src/grid/cursor_ops.rs	carriage_return_margin_mode_cursor_left_of_margin	carriage-return	
crates/aterm-grid/src/grid/cursor_ops.rs	carriage_return_margin_no_mode_goes_to_zero	carriage-return	
crates/aterm-grid/src/grid/cursor_ops.rs	carriage_return_moves_to_col_zero	carriage-return	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_backward_basic	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_backward_clears_pending_wrap	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_backward_from_zero	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_backward_margin_in_margins_stops_at_left	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_backward_margin_no_mode_same_as_backward	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_backward_margin_outside_margins_goes_to_zero	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_backward_stops_at_column_zero	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_down_above_scroll_region_stops_at_bottom_margin	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_down_basic	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_down_below_scroll_region_stops_at_last_line	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_down_clears_pending_wrap	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_down_stops_at_last_row_no_region	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_down_within_scroll_region_stops_at_bottom_margin	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_down_zero_does_nothing	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_forward_basic	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_forward_clears_pending_wrap	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_forward_margin_in_margins_stops_at_right	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_forward_margin_no_mode_same_as_forward	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_forward_margin_outside_margins_goes_to_edge	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_forward_stops_at_right_edge	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_forward_zero_does_nothing	cursor-horizontal-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_up_above_scroll_region_stops_at_zero	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_up_basic	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_up_below_scroll_region_stops_at_top_margin	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_up_clears_pending_wrap	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_up_stops_at_row_zero_no_region	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_up_within_scroll_region_stops_at_top_margin	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	cursor_up_zero_does_nothing	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	grid_1x1_every_motion_stays_at_origin	cursor-in-bounds	
crates/aterm-grid/src/grid/cursor_ops.rs	line_feed_at_bottom_of_region_scrolls	line-feed	
crates/aterm-grid/src/grid/cursor_ops.rs	line_feed_at_last_row_below_region_stays	line-feed	
crates/aterm-grid/src/grid/cursor_ops.rs	line_feed_below_region_moves_down	line-feed	
crates/aterm-grid/src/grid/cursor_ops.rs	line_feed_moves_down_within_region	line-feed	
crates/aterm-grid/src/grid/cursor_ops.rs	line_position_relative_ignores_bottom_margin	cursor-vertical-margins	
crates/aterm-grid/src/grid/cursor_ops.rs	move_cursor_by_clamps_to_max	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	move_cursor_by_clamps_to_zero	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	move_cursor_by_negative_offset	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	move_cursor_by_positive_offset	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	move_cursor_to_lands_or_clamps	cursor-set-clamp	
crates/aterm-grid/src/grid/cursor_ops.rs	restore_cursor_clamps_to_resized_grid	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	restore_cursor_without_save_is_noop	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	reverse_line_feed_above_region_moves_up	reverse-line-feed	
crates/aterm-grid/src/grid/cursor_ops.rs	reverse_line_feed_at_row_zero_above_region_stays	reverse-line-feed	
crates/aterm-grid/src/grid/cursor_ops.rs	reverse_line_feed_at_top_of_region_scrolls_down	reverse-line-feed	
crates/aterm-grid/src/grid/cursor_ops.rs	reverse_line_feed_clears_pending_wrap	reverse-line-feed	
crates/aterm-grid/src/grid/cursor_ops.rs	reverse_line_feed_moves_up_within_region	reverse-line-feed	
crates/aterm-grid/src/grid/cursor_ops.rs	save_and_restore_cursor_roundtrip	nonspec:delete	
crates/aterm-grid/src/grid/cursor_ops.rs	set_cursor_clamps_both_row_and_col	cursor-set-clamp	
crates/aterm-grid/src/grid/cursor_ops.rs	set_cursor_clamps_col_to_max	cursor-set-clamp	
crates/aterm-grid/src/grid/cursor_ops.rs	set_cursor_clamps_row_to_last_visible	cursor-set-clamp	
crates/aterm-grid/src/grid/cursor_ops.rs	set_cursor_clears_pending_wrap	cursor-set-clamp	
crates/aterm-grid/src/grid/cursor_ops.rs	set_cursor_exact_position	cursor-set-clamp	
crates/aterm-grid/src/grid/erase.rs	an_ed0_that_clears_the_last_double_row_lowers_the_fast_path_flag	ed-line-attributes	
crates/aterm-grid/src/grid/erase.rs	change_attrs_rect_maintains_len_both_directions	deccara-decrara	
crates/aterm-grid/src/grid/erase.rs	copy_rect_preserves_protected_cell_at_source_left_edge	deccra-copy	
crates/aterm-grid/src/grid/erase.rs	copy_rect_shrinks_dst_len_when_copied_blanks_empty_the_tail	deccra-copy	
crates/aterm-grid/src/grid/erase.rs	copy_rect_still_blanks_genuine_orphaned_wide_continuation_at_left_edge	deccra-copy	
crates/aterm-grid/src/grid/erase.rs	decsed_preserves_line_attributes_on_every_row_it_erases	ed-line-attributes	
crates/aterm-grid/src/grid/erase.rs	ed0_resets_the_complete_rows_below_and_leaves_the_cursor_row_alone	ed-line-attributes	
crates/aterm-grid/src/grid/erase.rs	ed1_resets_the_complete_rows_above_and_leaves_the_cursor_row_alone	ed-line-attributes	
crates/aterm-grid/src/grid/erase.rs	ed2_gives_back_the_columns_a_double_width_row_had_halved	ed-line-attributes	
crates/aterm-grid/src/grid/erase.rs	ed2_returns_every_erased_row_to_single_width	ed-line-attributes	
crates/aterm-grid/src/grid/erase.rs	el2_keeps_the_line_attribute_because_xterm_does	ed-line-attributes	
crates/aterm-grid/src/grid/erase.rs	erase_from_start_of_line_extends_the_bce_truecolor_ring_to_the_wide_orphan	erase-bce-orphan	
crates/aterm-grid/src/grid/erase.rs	erase_from_start_of_line_gives_the_wide_orphan_the_bce_background	erase-bce-orphan	
crates/aterm-grid/src/grid/erase.rs	erase_line_within_margins_gives_the_wide_orphan_the_bce_background_and_ring	erase-bce-orphan	
crates/aterm-grid/src/grid/erase.rs	erase_rect_keeps_the_wide_orphan_empty_under_a_bce_background	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	erase_rect_preserves_protected_cell_left_of_the_region	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	erase_to_end_of_line_extends_the_bce_truecolor_ring_to_the_wide_orphan	erase-bce-orphan	
crates/aterm-grid/src/grid/erase.rs	erase_to_end_of_line_gives_the_wide_orphan_the_bce_background	erase-bce-orphan	
crates/aterm-grid/src/grid/erase.rs	fill_rect_keeps_the_wide_orphan_empty	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	reverse_attrs_of_underline_family_never_yields_an_unnamed_style	reverse-cell-attrs	
crates/aterm-grid/src/grid/erase.rs	reverse_attrs_rect_maintains_len_both_directions	deccara-decrara	
crates/aterm-grid/src/grid/erase.rs	test_change_attrs_rect_clears_flags	deccara-decrara	
crates/aterm-grid/src/grid/erase.rs	test_change_attrs_rect_keeps_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_change_attrs_rect_sets_bold	deccara-decrara	
crates/aterm-grid/src/grid/erase.rs	test_copy_rect_basic	deccra-copy	
crates/aterm-grid/src/grid/erase.rs	test_copy_rect_clears_stale_dst_complex_ring	deccra-copy	
crates/aterm-grid/src/grid/erase.rs	test_copy_rect_inverted_source_noop	deccra-copy	
crates/aterm-grid/src/grid/erase.rs	test_copy_rect_keeps_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_copy_rect_repopulates_truecolor_and_complex_rings	deccra-copy	
crates/aterm-grid/src/grid/erase.rs	test_decaln_cells_have_default_attributes	decaln	
crates/aterm-grid/src/grid/erase.rs	test_decaln_clears_any_double_width_flag	decaln	
crates/aterm-grid/src/grid/erase.rs	test_decaln_clears_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_decaln_fills_entire_screen_with_e	decaln	
crates/aterm-grid/src/grid/erase.rs	test_decaln_on_1x1_grid	decaln	
crates/aterm-grid/src/grid/erase.rs	test_decaln_overwrites_existing_content	decaln	
crates/aterm-grid/src/grid/erase.rs	test_decaln_resets_cursor_to_home	decaln	
crates/aterm-grid/src/grid/erase.rs	test_decaln_resets_scroll_region	decaln	
crates/aterm-grid/src/grid/erase.rs	test_decsed0_clears_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_decsed0_erases_unprotected_to_end_of_screen	selective-erase-span	
crates/aterm-grid/src/grid/erase.rs	test_decsed0_one_column_all_protected_at_origin_keeps_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_decsed0_one_column_one_unprotected_cell_at_origin_resets_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_decsed0_protected_parked_cell_survives_but_wrap_resets	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_decsed1_all_protected_span_keeps_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_decsed1_bottom_right_fully_protected_screen_keeps_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_decsed1_clears_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_decsed1_erases_unprotected_from_start_of_screen	selective-erase-span	
crates/aterm-grid/src/grid/erase.rs	test_decsed2_clears_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_decsed2_erases_unprotected_entire_screen	selective-erase-span	
crates/aterm-grid/src/grid/erase.rs	test_decsed2_fully_protected_screen_keeps_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_ed0_at_pending_wrap_clears_parked_glyph_and_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_ed0_cursor_at_last_row_last_col	ed-span	
crates/aterm-grid/src/grid/erase.rs	test_ed0_cursor_at_top_left_erases_everything	ed-span	
crates/aterm-grid/src/grid/erase.rs	test_ed0_erases_from_cursor_to_end_of_screen	ed-span	
crates/aterm-grid/src/grid/erase.rs	test_ed0_on_single_row_grid	ed-span	
crates/aterm-grid/src/grid/erase.rs	test_ed0_one_column_grid_pending_wrap_at_origin_clears_screen_and_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_ed1_bottom_right_pending_wrap_clears_screen_and_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_ed1_clears_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_ed1_cursor_at_bottom_right_erases_everything	ed-span	
crates/aterm-grid/src/grid/erase.rs	test_ed1_erases_from_start_of_screen_to_cursor	ed-span	
crates/aterm-grid/src/grid/erase.rs	test_ed2_clears_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_ed2_erases_entire_screen	ed-span	
crates/aterm-grid/src/grid/erase.rs	test_ed2_resets_display_offset	ed-span	
crates/aterm-grid/src/grid/erase.rs	test_el0_at_pending_wrap_clears_parked_glyph_and_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_el0_breaks_the_wrap_into_the_row_below_as_xterm_clear_right_does	right-erase-breaks-wrap-link	
crates/aterm-grid/src/grid/erase.rs	test_el0_cursor_at_column_zero_erases_entire_line	el-span	
crates/aterm-grid/src/grid/erase.rs	test_el0_cursor_at_last_column	el-span	
crates/aterm-grid/src/grid/erase.rs	test_el0_does_not_affect_other_rows	el-span	
crates/aterm-grid/src/grid/erase.rs	test_el0_erases_from_cursor_to_end	el-span	
crates/aterm-grid/src/grid/erase.rs	test_el0_wide_glyph_parked_on_continuation_clears_both_halves_and_wrap	el-span	
crates/aterm-grid/src/grid/erase.rs	test_el1_clears_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_el1_cursor_at_column_zero	el-span	
crates/aterm-grid/src/grid/erase.rs	test_el1_cursor_at_last_column_erases_entire_line	el-span	
crates/aterm-grid/src/grid/erase.rs	test_el1_el2_and_el0_on_the_continuation_keep_the_link	right-erase-breaks-wrap-link	
crates/aterm-grid/src/grid/erase.rs	test_el1_erases_from_start_to_cursor	el-span	
crates/aterm-grid/src/grid/erase.rs	test_el2_at_pending_wrap_leaves_the_row_below_intact	el-span	
crates/aterm-grid/src/grid/erase.rs	test_el2_clears_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_el2_does_not_affect_other_rows	el-span	
crates/aterm-grid/src/grid/erase.rs	test_el2_erases_entire_line	el-span	
crates/aterm-grid/src/grid/erase.rs	test_erase_line_on_1x1_grid	el-span	
crates/aterm-grid/src/grid/erase.rs	test_erase_on_1x1_grid	el-span	
crates/aterm-grid/src/grid/erase.rs	test_erase_rect_clamps_to_bounds	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	test_erase_rect_clears_region	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	test_erase_rect_full_screen	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	test_erase_rect_inverted_coords_noop	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	test_erase_rect_keeps_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_erase_rect_single_cell	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	test_erase_screen_on_1x1_grid	ed-span	
crates/aterm-grid/src/grid/erase.rs	test_erase_scrollback_keeps_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_erase_scrollback_no_scrollback	ed3-erase-scrollback	
crates/aterm-grid/src/grid/erase.rs	test_fill_rect_does_not_affect_surrounding	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	test_fill_rect_fills_with_character	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	test_fill_rect_keeps_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_fill_rect_preserves_cell_attributes	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	test_fill_rect_repopulates_truecolor_ring	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	test_fill_rect_single_cell	rect-fill-erase	
crates/aterm-grid/src/grid/erase.rs	test_multiple_erases_in_sequence	el-span	
crates/aterm-grid/src/grid/erase.rs	test_sel0_all_protected_nothing_erased	selective-erase-span	
crates/aterm-grid/src/grid/erase.rs	test_sel0_clears_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_sel0_erases_unprotected_from_cursor_to_end	selective-erase-span	
crates/aterm-grid/src/grid/erase.rs	test_sel0_protected_parked_cell_survives_but_wrap_resets	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_sel1_all_protected_span_keeps_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_sel1_clears_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_sel1_erases_unprotected_from_start_to_cursor	selective-erase-span	
crates/aterm-grid/src/grid/erase.rs	test_sel1_one_unprotected_cell_resets_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_sel2_all_protected_row_keeps_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_sel2_clears_pending_wrap	selective-erase-pending-wrap	
crates/aterm-grid/src/grid/erase.rs	test_sel2_erases_unprotected_entire_line	selective-erase-span	
crates/aterm-grid/src/grid/erase.rs	test_selective_erase_no_protected_cells_erases_all	selective-erase-span	
crates/aterm-grid/src/grid/erase.rs	test_selective_erase_rect_keeps_pending_wrap	erase-pending-wrap-table	
crates/aterm-grid/src/grid/erase.rs	test_selective_erase_rect_preserves_truecolor_extra	decsera	
crates/aterm-grid/src/grid/erase.rs	test_selective_erase_rect_respects_protection	decsera	
crates/aterm-grid/src/grid/erase.rs	test_selective_erase_rect_scrubs_combining_marks	decsera	
crates/aterm-grid/src/grid/erase.rs	test_selective_erase_rect_wide_cell_leaves_no_orphan_continuation	decsera	
crates/aterm-grid/src/grid/history_carry_tests.rs	a_build_for_another_width_rewraps	older-history-build	
crates/aterm-grid/src/grid/history_carry_tests.rs	a_continued_numbering_reserves_only_the_shortfall	reserve-older-history-keys	
crates/aterm-grid/src/grid/history_carry_tests.rs	a_fenced_export_reads_every_history_line_in_order_across_every_tier	history-fence-export	
crates/aterm-grid/src/grid/history_carry_tests.rs	a_ring_only_grid_has_nothing_to_place_history_in_front_of	attach-older-history	
crates/aterm-grid/src/grid/history_carry_tests.rs	a_rows_grow_that_reveals_history_breaks_the_fence	history-fence-breaks	
crates/aterm-grid/src/grid/history_carry_tests.rs	a_scrollback_clear_breaks_the_fence	history-fence-breaks	
crates/aterm-grid/src/grid/history_carry_tests.rs	a_scrollback_cleared_after_the_reserve_refuses_the_import	attach-older-history	
crates/aterm-grid/src/grid/history_carry_tests.rs	a_width_reflow_breaks_the_fence	history-fence-breaks	
crates/aterm-grid/src/grid/history_carry_tests.rs	a_width_that_moved_since_the_build_is_refused_and_the_history_handed_back	attach-older-history	
crates/aterm-grid/src/grid/history_carry_tests.rs	an_unreserved_import_raises_the_counter_and_says_it_renumbered	attach-older-history	
crates/aterm-grid/src/grid/history_carry_tests.rs	eviction_past_the_reader_is_evicted_not_a_gap	history-fence-breaks	
crates/aterm-grid/src/grid/history_carry_tests.rs	older_history_lands_before_the_oldest_line_and_keeps_every_key	attach-older-history	
crates/aterm-grid/src/grid/history_carry_tests.rs	output_after_the_fence_leaves_it_standing_and_stays_outside_it	history-fence-export	
crates/aterm-grid/src/grid/history_carry_tests.rs	the_grids_own_retention_decides_what_an_import_keeps	attach-older-history	
crates/aterm-grid/src/grid/line_ops.rs	test_dch_at_beginning_of_line	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_dch_at_last_column	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_dch_basic_delete_one	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_dch_clears_pending_wrap	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_dch_count_exceeds_remaining	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_dch_delete_multiple	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_dch_does_not_affect_other_rows	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_dch_on_empty_row	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_dch_with_wide_char	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_decdc_cursor_row_outside_region_is_noop	decic-decdc	
crates/aterm-grid/src/grid/line_ops.rs	test_decic_cursor_row_inside_region_still_inserts	decic-decdc	
crates/aterm-grid/src/grid/line_ops.rs	test_decic_cursor_row_outside_region_is_noop	decic-decdc	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_at_bottom_of_scroll_region	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_at_top	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_basic_delete_one	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_count_exceeds_region	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_cursor_moves_to_col0	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_delete_multiple	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_margined_cursor_col_outside_margins_is_noop	il-dl-margined	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_margined_shrinks_len_when_blanks_cover_tail	il-dl-margined	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_outside_scroll_region_is_noop	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_preserves_rows_above_cursor	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_single_row_grid	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_within_scroll_region	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_dl_zero_count_is_noop	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_at_beginning	ech-erase-chars	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_basic_erase	ech-erase-chars	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_clears_pending_wrap	ech-erase-chars	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_count_exceeds_remaining	ech-erase-chars	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_does_not_affect_other_rows	ech-erase-chars	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_does_not_shift_content	ech-erase-chars	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_erase_one	ech-erase-chars	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_on_empty_row	ech-erase-chars	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_on_wide_char	ech-erase-chars	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_short_of_the_last_column_keeps_the_wrap	right-erase-breaks-wrap-link	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_through_the_last_column_breaks_the_wrap_as_el0_does	right-erase-breaks-wrap-link	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_under_margins_runs_to_the_row_end_and_breaks_the_wrap_as_el0_does	right-erase-breaks-wrap-link	
crates/aterm-grid/src/grid/line_ops.rs	test_ech_under_margins_short_of_the_row_end_keeps_the_wrap	right-erase-breaks-wrap-link	
crates/aterm-grid/src/grid/line_ops.rs	test_ich_at_beginning_of_line	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_ich_at_last_column	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_ich_basic_insert_one	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_ich_clears_pending_wrap	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_ich_count_exceeds_remaining	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_ich_does_not_affect_other_rows	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_ich_insert_multiple	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_ich_on_empty_row	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_ich_then_dch_roundtrip	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_ich_with_wide_char	ich-dch-shift	
crates/aterm-grid/src/grid/line_ops.rs	test_il_at_top	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_at_top_of_scroll_region	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_basic_insert_one	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_count_exceeds_region	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_cursor_moves_to_col0	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_insert_multiple	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_margined_cursor_col_outside_margins_is_noop	il-dl-margined	
crates/aterm-grid/src/grid/line_ops.rs	test_il_margined_cursor_inside_margins_still_shifts	il-dl-margined	
crates/aterm-grid/src/grid/line_ops.rs	test_il_outside_scroll_region_is_noop	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_preserves_rows_above_cursor	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_single_row_grid	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_then_dl_roundtrip	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_within_scroll_region	il-dl	
crates/aterm-grid/src/grid/line_ops.rs	test_il_zero_count_is_noop	il-dl	
crates/aterm-grid/src/grid/pin_tests.rs	continue_absolute_numbering_is_bounded_both_ways	continue-absolute-numbering	
crates/aterm-grid/src/grid/pin_tests.rs	evict_pages_from	nonspec:delete	
crates/aterm-grid/src/grid/pin_tests.rs	generation_tracker_evict	nonspec:delete	
crates/aterm-grid/src/grid/pin_tests.rs	generation_tracker_new	nonspec:delete	
crates/aterm-grid/src/grid/pin_tests.rs	pin_creation	nonspec:delete	
crates/aterm-grid/src/grid/pin_tests.rs	pin_from_absolute	nonspec:delete	
crates/aterm-grid/src/grid/pin_tests.rs	pin_validity	nonspec:delete	
crates/aterm-grid/src/grid/pin_tests.rs	pin_with_modifications	nonspec:delete	
crates/aterm-grid/src/grid/pin_tests.rs	pinned_range	nonspec:delete	
crates/aterm-grid/src/grid/pin_tests.rs	pinned_range_normalized	nonspec:delete	
crates/aterm-grid/src/grid/pin_tests.rs	pinned_range_validity	nonspec:delete	
crates/aterm-grid/src/grid/reflow.rs	conpty_rewrap_carries_a_cursor_on_a_blank_row_past_the_width	reflow-cursor-tracking	
crates/aterm-grid/src/grid/reflow.rs	conpty_rewrap_keeps_the_cursor_cell_past_the_prompt_as_a_row	reflow-cursor-tracking	
crates/aterm-grid/src/grid/reflow.rs	conpty_rewrap_keeps_the_cursor_offset_on_an_unwrapped_line	reflow-cursor-tracking	
crates/aterm-grid/src/grid/reflow.rs	conpty_rows_grow_appends_blanks_and_reveals_nothing	rows-grow-policy	
crates/aterm-grid/src/grid/reflow.rs	conpty_rows_grow_inside_an_offload_window_keeps_the_reader_aim	offload-reader-position	
crates/aterm-grid/src/grid/reflow.rs	conpty_rows_grow_then_trim_keeps_every_key	rows-grow-policy	
crates/aterm-grid/src/grid/reflow.rs	conpty_rows_shrink_equals_native	rows-shrink-shape	
crates/aterm-grid/src/grid/reflow.rs	conpty_widen_skips_the_boundary_lift_and_the_deficit_fill	width-change-seam-policy	
crates/aterm-grid/src/grid/reflow.rs	conpty_width_round_trip_keeps_the_history_line_count	width-change-seam-policy	
crates/aterm-grid/src/grid/reflow.rs	conpty_width_shrink_demotes_no_further_than_the_cursor_and_drops_blanks	conpty-width-demote	
crates/aterm-grid/src/grid/reflow.rs	conpty_width_shrink_demotes_the_excess_above_a_non_blank_row	conpty-width-demote	
crates/aterm-grid/src/grid/reflow.rs	last_resize_shape_in_the_trim_demote_and_bottom_push_regimes	rows-shrink-shape	
crates/aterm-grid/src/grid/reflow.rs	native_rows_grow_still_reveals_history	rows-grow-policy	
crates/aterm-grid/src/grid/reflow.rs	native_widen_still_rejoins_and_refills	width-change-seam-policy	
crates/aterm-grid/src/grid/reflow.rs	reflow_cursor_tracking_through_shrink_grow_roundtrip	reflow-cursor-tracking	
crates/aterm-grid/src/grid/reflow.rs	reflow_disabled_does_not_wrap	no-reflow-resize-truncates	
crates/aterm-grid/src/grid/reflow.rs	reflow_disabled_grow_does_not_unwrap	no-reflow-resize-truncates	
crates/aterm-grid/src/grid/reflow.rs	reflow_grow_from_1_column	reflow-rewrap-layout	
crates/aterm-grid/src/grid/reflow.rs	reflow_grow_merges_only_soft_wrapped	reflow-rewrap-layout	
crates/aterm-grid/src/grid/reflow.rs	reflow_mode_from_false	reflow-mode-from-bool	
crates/aterm-grid/src/grid/reflow.rs	reflow_mode_from_true	reflow-mode-from-bool	
crates/aterm-grid/src/grid/reflow.rs	reflow_same_width_is_identity	reflow-rewrap-layout	
crates/aterm-grid/src/grid/reflow.rs	reflow_shrink_multiple_lines	reflow-rewrap-layout	
crates/aterm-grid/src/grid/reflow.rs	reflow_shrink_single_char_line	reflow-rewrap-layout	
crates/aterm-grid/src/grid/reflow.rs	reflow_shrink_to_1_column	reflow-rewrap-layout	
crates/aterm-grid/src/grid/reflow.rs	resize_clamps_oversize_dimensions	resize-clamp-and-reset	
crates/aterm-grid/src/grid/reflow.rs	resize_no_reflow_clamps_oversize_dimensions	resize-clamp-and-reset	
crates/aterm-grid/src/grid/reflow_map.rs	a_chunk_backed_off_a_wide_glyph_records_its_hole	chunk-cells-to-rows	
crates/aterm-grid/src/grid/reflow_map.rs	a_printed_space_at_a_chunk_end_stays_inside_the_rows_length	chunk-cells-to-rows	
crates/aterm-grid/src/grid/reflow_map.rs	adjust_chunk_boundary_at_end_of_cells	adjust-chunk-boundary	
crates/aterm-grid/src/grid/reflow_map.rs	adjust_chunk_boundary_empty_cells	adjust-chunk-boundary	
crates/aterm-grid/src/grid/reflow_map.rs	adjust_chunk_boundary_forward_progress_single_cell	adjust-chunk-boundary	
crates/aterm-grid/src/grid/reflow_map.rs	adjust_chunk_boundary_no_wide_chars	adjust-chunk-boundary	
crates/aterm-grid/src/grid/reflow_map.rs	adjust_chunk_boundary_single_ascii_cell	adjust-chunk-boundary	
crates/aterm-grid/src/grid/reflow_map.rs	adjust_chunk_boundary_wide_char_at_boundary	adjust-chunk-boundary	
crates/aterm-grid/src/grid/reflow_map.rs	adjust_chunk_boundary_wide_char_not_at_boundary	adjust-chunk-boundary	
crates/aterm-grid/src/grid/reflow_map.rs	chunk_cells_to_rows_cursor_on_last_chunk_boundary	chunk-cells-to-rows	
crates/aterm-grid/src/grid/reflow_map.rs	chunk_cells_to_rows_multi_chunk	chunk-cells-to-rows	
crates/aterm-grid/src/grid/reflow_map.rs	chunk_cells_to_rows_single_chunk	chunk-cells-to-rows	
crates/aterm-grid/src/grid/reflow_map.rs	chunk_cells_to_rows_single_column	chunk-cells-to-rows	
crates/aterm-grid/src/grid/reflow_map.rs	chunk_cells_to_rows_tracks_two_cursors	chunk-cells-to-rows	
crates/aterm-grid/src/grid/reflow_map.rs	copied_extra_preserves_full_strings_and_resolves_ring_colors	remap-copied-extra	
crates/aterm-grid/src/grid/reflow_map.rs	copy_cells_to_row_basic_ascii	copy-cells-to-row	
crates/aterm-grid/src/grid/reflow_map.rs	copy_cells_to_row_empty_range	copy-cells-to-row	
crates/aterm-grid/src/grid/reflow_map.rs	copy_cells_to_row_wide_char_in_narrow_terminal	copy-cells-to-row	
crates/aterm-grid/src/grid/reflow_map.rs	copy_cells_to_row_with_offset	copy-cells-to-row	
crates/aterm-grid/src/grid/reflow_map.rs	cursor_in_chunk_after_range	cursor-in-chunk	
crates/aterm-grid/src/grid/reflow_map.rs	cursor_in_chunk_at_end_last_chunk	cursor-in-chunk	
crates/aterm-grid/src/grid/reflow_map.rs	cursor_in_chunk_at_end_non_last	cursor-in-chunk	
crates/aterm-grid/src/grid/reflow_map.rs	cursor_in_chunk_at_start	cursor-in-chunk	
crates/aterm-grid/src/grid/reflow_map.rs	cursor_in_chunk_before_range	cursor-in-chunk	
crates/aterm-grid/src/grid/reflow_map.rs	cursor_in_chunk_clamped_to_cells_len	cursor-in-chunk	
crates/aterm-grid/src/grid/reflow_map.rs	cursor_in_chunk_within_range	cursor-in-chunk	
crates/aterm-grid/src/grid/reflow_map.rs	cursor_in_chunk_zero_length_cells	cursor-in-chunk	
crates/aterm-grid/src/grid/reflow_map.rs	source_coords_for_row_basic	source-coords-for-row	
crates/aterm-grid/src/grid/reflow_map.rs	source_coords_for_row_empty	source-coords-for-row	
crates/aterm-grid/src/grid/reflow_map.rs	source_coords_for_row_single_cell	source-coords-for-row	
crates/aterm-grid/src/grid/reflow_map.rs	source_coords_for_row_wide_row	source-coords-for-row	
crates/aterm-grid/src/grid/resize_undo.rs	a_quiet_52_51_52_flap_is_an_identity	resize-undo-flap-identity	
crates/aterm-grid/src/grid/resize_undo.rs	a_retaining_grid_keeps_no_undo	resize-undo-validity	
crates/aterm-grid/src/grid/resize_undo.rs	a_trim_stacked_on_a_push_comes_back_above_it	resize-undo-flap-identity	
crates/aterm-grid/src/grid/resize_undo.rs	a_write_a_cursor_move_or_a_width_change_drops_the_undo	resize-undo-validity	
crates/aterm-grid/src/grid/resize_undo.rs	pushed_and_trimmed_rows_come_back_where_they_were	resize-undo-flap-identity	
crates/aterm-grid/src/grid/resize_undo.rs	stacked_shrinks_and_partial_grows_unwind_in_order	resize-undo-flap-identity	
crates/aterm-grid/src/grid/scroll.rs	a_120k_burst_into_a_100k_scrollback_keeps_the_newest_100k_with_no_hole	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/scroll.rs	a_cut_beneath_promoted_lines_sits_exactly_where_the_hole_is	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/scroll.rs	a_cut_deeper_than_the_limit_names_only_what_the_limit_would_keep	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/scroll.rs	a_drain_promotes_the_marker_first_and_a_later_drop_opens_a_new_cut	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/scroll.rs	a_drained_flood_backlog_gives_its_spine_back	lazy-spine-slack-bound	
crates/aterm-grid/src/grid/scroll.rs	a_full_width_flood_is_cut_at_the_memory_headroom_and_marked_exactly	flood-backlog-cap	
crates/aterm-grid/src/grid/scroll.rs	a_marker_shrinks_as_the_window_slides_past_its_hole	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/scroll.rs	a_short_line_flood_is_weighed_by_its_cells_not_the_window_width	flood-backlog-cap	
crates/aterm-grid/src/grid/scroll.rs	a_slow_tail_after_a_flood_is_promoted_not_dropped	trickle-skip-rule	
crates/aterm-grid/src/grid/scroll.rs	a_trickle_that_skips_an_open_cut_leaves_one_marker_per_flood	trickle-skip-rule	
crates/aterm-grid/src/grid/scroll.rs	a_trickling_worker_is_evicted_ahead_of_the_flood_not_cut_around	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/scroll.rs	a_window_that_slides_past_the_whole_hole_takes_the_marker	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/scroll.rs	no_marker_when_nothing_was_dropped	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/scroll.rs	ordinary_top_eviction_is_neither_counted_nor_marked	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/scroll.rs	the_marker_states_the_count_first_and_fits_the_default_width	flood-marker-line	
crates/aterm-grid/src/grid/scroll.rs	with_no_line_limit_every_drop_is_a_loss	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/scroll_convert.rs	armed_ring_does_not_change_a_plain_row	extract-row-extras-gate	
crates/aterm-grid/src/grid/scroll_convert.rs	is_empty_iff_every_field_is_empty	scrolled-row-extras-is-empty	
crates/aterm-grid/src/grid/scroll_convert.rs	map_empty_row_with_ring_rgb_still_extracts	extract-row-extras-gate	
crates/aterm-grid/src/grid/scroll_convert.rs	plain_run_equivalence_rejects_lost_text_attributes_and_wrap	nonspec:delete	
crates/aterm-grid/src/grid/scroll_convert.rs	plain_run_materialization_matches_old_loop_across_fallbacks	deferred-line-equals-eager	
crates/aterm-grid/src/grid/scroll_convert.rs	plain_run_materialization_matches_old_loop_at_chunk_boundaries	deferred-line-equals-eager	
crates/aterm-grid/src/grid/scroll_fill.rs	a_glyph_the_row_cannot_hold_takes_its_link_and_underline_colour	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	fill_row_hyperlinks_disjoint_are_correct_and_overlap_is_bounded	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	test_fill_row_different_row_index	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	test_fill_row_from_ascii_line	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	test_fill_row_from_empty_line	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	test_fill_row_from_line_clears_previous_content	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	test_fill_row_from_line_not_wrapped	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	test_fill_row_from_line_preserves_wrapped_flag	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	test_fill_row_from_line_with_default_attrs	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	test_fill_row_out_of_bounds_row_is_noop	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	test_fill_row_truncates_to_cols	fill-row-from-line	
crates/aterm-grid/src/grid/scroll_fill.rs	test_multiple_scroll_down	scroll-region-shift	
crates/aterm-grid/src/grid/scroll_fill.rs	test_multiple_scroll_up	scroll-region-shift	
crates/aterm-grid/src/grid/scroll_fill.rs	test_scroll_down_content_shifts_down	scroll-region-shift	
crates/aterm-grid/src/grid/scroll_fill.rs	test_scroll_down_count_exceeds_region	scroll-region-shift	
crates/aterm-grid/src/grid/scroll_fill.rs	test_scroll_region_down_within_region	scroll-region-shift	
crates/aterm-grid/src/grid/scroll_fill.rs	test_scroll_region_full_screen_delegates_to_scroll_up	scroll-region-shift	
crates/aterm-grid/src/grid/scroll_fill.rs	test_scroll_region_up_within_region	scroll-region-shift	
crates/aterm-grid/src/grid/scroll_fill.rs	test_scroll_up_content_shifts_up	scroll-region-shift	
crates/aterm-grid/src/grid/scroll_fill.rs	test_scroll_up_count_exceeds_region	scroll-region-shift	
crates/aterm-grid/src/grid/scroll_fill.rs	test_scroll_up_zero_is_noop	scroll-region-shift	
crates/aterm-grid/src/grid/scroll_materialize.rs	ascii_fast_path_matches_the_scan	grapheme-ascii-fast-path	
crates/aterm-grid/src/grid/scroll_materialize.rs	the_search_walk_lays_a_line_out_as_materialize_does	stored-cells-run-expansion	
crates/aterm-grid/src/grid/scrollback_access.rs	get_history_line_returns_none_on_error	history-read-errors	
crates/aterm-grid/src/grid/scrollback_access.rs	search_content_returns_none_on_corrupted_scrollback	history-read-errors	
crates/aterm-grid/src/grid/scrollback_access.rs	try_get_history_line_propagates_scrollback_error	history-read-errors	
crates/aterm-grid/src/grid/scrollback_access.rs	try_history_line_rev_propagates_scrollback_error	history-read-errors	
crates/aterm-grid/src/grid/scrollback_offload.rs	a_job_rewraps_a_line_straddling_the_store_ring_seam_whole	offload-refines-plain-resize	
crates/aterm-grid/src/grid/scrollback_offload.rs	abort_never_expands_fallback_ring_for_raise_or_unlimited	offload-window-refinement	
crates/aterm-grid/src/grid/scrollback_offload.rs	abort_preserves_staged_output_when_replacement_store_exists	offload-window-refinement	
crates/aterm-grid/src/grid/scrollback_offload.rs	attached_memory_budget_settles_lazy_rows_under_new_budget	offload-window-refinement	
crates/aterm-grid/src/grid/scrollback_offload.rs	clean_detach_snapshot_adopts_replacement_store_settings	offload-window-refinement	
crates/aterm-grid/src/grid/scrollback_offload.rs	converging_reattach_rewraps_to_the_settled_width	offload-refines-plain-resize	
crates/aterm-grid/src/grid/scrollback_offload.rs	converging_redetach_stages_ring_and_window_output_without_ui_store_push	offload-refines-plain-resize	
crates/aterm-grid/src/grid/scrollback_offload.rs	detached_line_limit_applies_to_ring_after_abort	offload-window-refinement	
crates/aterm-grid/src/grid/scrollback_offload.rs	detached_settings_apply_on_normal_reattach	offload-window-refinement	
crates/aterm-grid/src/grid/scrollback_offload.rs	detached_settings_apply_to_replacement_store_race	offload-window-refinement	
crates/aterm-grid/src/grid/scrollback_offload.rs	detached_settings_survive_clear_before_reattach	offload-window-refinement	
crates/aterm-grid/src/grid/scrollback_offload.rs	detached_unlimited_limit_round_trips_on_reattach	offload-window-refinement	
crates/aterm-grid/src/grid/scrollback_offload.rs	dropping_a_half_stepped_job_recovers_via_abort	offload-window-refinement	
crates/aterm-grid/src/grid/scrollback_offload.rs	offload_round_trip_matches_plain_resize_history_sequence	offload-refines-plain-resize	
crates/aterm-grid/src/grid/scrollback_offload.rs	reflow_progress_climbs_to_the_detached_total	offload-step-schedule-independence	
crates/aterm-grid/src/grid/scrollback_offload.rs	reflow_step_any_schedule_matches_one_shot	offload-step-schedule-independence	
crates/aterm-grid/src/grid/scrollback_offload.rs	reflow_step_budget_one_never_splits_a_soft_wrapped_run	offload-step-schedule-independence	
crates/aterm-grid/src/grid/scrollback_offload.rs	reflow_step_empty_history_terminates	offload-step-schedule-independence	
crates/aterm-grid/src/grid/scrollback_offload.rs	reflow_step_timing	nonspec:move-to-bench	
crates/aterm-grid/src/grid/scrollback_offload.rs	reflow_step_zero_budget_still_makes_progress	offload-step-schedule-independence	
crates/aterm-grid/src/grid/scrollback_offload.rs	stepping_interleaved_with_window_output_keeps_history_and_order	offload-refines-plain-resize	
crates/aterm-grid/src/grid/scrollback_offload.rs	streaming_matches_materialize_all_reference	offload-step-schedule-independence	
crates/aterm-grid/src/grid/scrollback_offload.rs	streaming_step_carry_stays_bounded	offload-step-schedule-independence	
crates/aterm-grid/src/grid/scrollback_offload.rs	streaming_under_byte_budget_pressure_keeps_a_longer_suffix	offload-step-schedule-independence	
crates/aterm-grid/src/grid/scrollback_reflow.rs	reflow_newest_rows_is_the_tail_of_the_whole_rewrap	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	reflow_passthrough_matches_full_path_on_mixed_corpus	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	reflow_passthrough_never_fires_on_wrap_changing_input	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_blank_logical_line_survives	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_grow_merges_soft_wrapped_run	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_hyperlink_gap_cell_has_no_link	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_preserves_disjoint_hyperlinks_across_a_split	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_preserves_hard_newlines	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_preserves_stored_ambiguous_width_with_narrow_control	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_preserves_underline_colors_across_a_split	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_round_trip_is_content_stable	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_shrink_splits_logical_line	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_underline_color_gap_cell_has_none	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_vs15_overrides_inconsistent_stored_wide_flag	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/scrollback_reflow.rs	rewrap_wide_char_not_split_across_rows	scrollback-rewrap-denotation	
crates/aterm-grid/src/grid/state/storage.rs	a_row_replaced_by_a_fresh_allocation_is_seen_by_both_readings	nonspec:delete	
crates/aterm-grid/src/grid/state/storage.rs	attach_scrollback_sets_some	retention-limit	
crates/aterm-grid/src/grid/state/storage.rs	clamp_col_for_row_at_max	column-geometry	
crates/aterm-grid/src/grid/state/storage.rs	clamp_col_for_row_exceeds_max	column-geometry	
crates/aterm-grid/src/grid/state/storage.rs	clamp_col_for_row_within_bounds	column-geometry	
crates/aterm-grid/src/grid/state/storage.rs	cursor_save_and_restore	nonspec:delete	
crates/aterm-grid/src/grid/state/storage.rs	cursor_take_pending_wrap_returns_false_when_not_set	pending-wrap-motion-table	
crates/aterm-grid/src/grid/state/storage.rs	cursor_take_pending_wrap_returns_true_and_clears	pending-wrap-motion-table	
crates/aterm-grid/src/grid/state/storage.rs	effective_cols_double_height_bottom	column-geometry	
crates/aterm-grid/src/grid/state/storage.rs	effective_cols_double_height_top	column-geometry	
crates/aterm-grid/src/grid/state/storage.rs	effective_cols_double_width_row	column-geometry	
crates/aterm-grid/src/grid/state/storage.rs	horizontal_margins_invalid_resets_to_full	region-and-margin-setters	
crates/aterm-grid/src/grid/state/storage.rs	horizontal_margins_reset_clears_flag	region-and-margin-setters	
crates/aterm-grid/src/grid/state/storage.rs	horizontal_margins_set_custom	region-and-margin-setters	
crates/aterm-grid/src/grid/state/storage.rs	max_col_double_width	column-geometry	
crates/aterm-grid/src/grid/state/storage.rs	push_ring_extras_empty	grid-type-invariant	
crates/aterm-grid/src/grid/state/storage.rs	resize_viewport_clamps_cursor	resize-clamp-and-reset	
crates/aterm-grid/src/grid/state/storage.rs	resize_viewport_clears_pending_wrap	resize-clamp-and-reset	
crates/aterm-grid/src/grid/state/storage.rs	resize_viewport_grows_tab_stops	resize-clamp-and-reset	
crates/aterm-grid/src/grid/state/storage.rs	resize_viewport_leaves_the_saved_cursor_to_its_reader	resize-clamp-and-reset	
crates/aterm-grid/src/grid/state/storage.rs	resize_viewport_resets_horizontal_margins	resize-clamp-and-reset	
crates/aterm-grid/src/grid/state/storage.rs	resize_viewport_resets_scroll_region	resize-clamp-and-reset	
crates/aterm-grid/src/grid/state/storage.rs	resize_viewport_shrink_preserves_tab_stops	resize-clamp-and-reset	
crates/aterm-grid/src/grid/state/storage.rs	row_access_out_of_bounds_returns_none	row-access-bounds	
crates/aterm-grid/src/grid/state/storage.rs	row_mut_out_of_bounds_returns_none	row-access-bounds	
crates/aterm-grid/src/grid/state/storage.rs	row_mut_with_effective_cols_out_of_bounds	row-access-bounds	
crates/aterm-grid/src/grid/state/storage.rs	scroll_region_invalid_range_resets_to_full	region-and-margin-setters	
crates/aterm-grid/src/grid/state/storage.rs	scrollback_line_limit	retention-limit	
crates/aterm-grid/src/grid/state/storage.rs	shift_rows_down_n_exceeds_range_is_noop	ring-row-shift-permutation	
crates/aterm-grid/src/grid/state/storage.rs	shift_rows_down_n_zero_is_noop	ring-row-shift-permutation	
crates/aterm-grid/src/grid/state/storage.rs	shift_rows_up_n_exceeds_range_is_noop	ring-row-shift-permutation	
crates/aterm-grid/src/grid/state/storage.rs	shift_rows_up_n_zero_is_noop	ring-row-shift-permutation	
crates/aterm-grid/src/grid/state/storage.rs	shift_visible_rows_permutes_row_slices_and_allocates_nothing_at_capacity	ring-row-shift-permutation	
crates/aterm-grid/src/grid/state/storage.rs	tab_stops_clear_all	tab-stop-set-clear	
crates/aterm-grid/src/grid/state/storage.rs	tab_stops_reset	tab-default-stops	
crates/aterm-grid/src/grid/tests/algorithm_boundary/erase.rs	erase_from_start_of_screen_cursor_at_last_row	ed-span	
crates/aterm-grid/src/grid/tests/algorithm_boundary/erase.rs	erase_from_start_of_screen_cursor_at_origin	ed-span	
crates/aterm-grid/src/grid/tests/algorithm_boundary/erase.rs	erase_to_end_of_screen_cursor_at_last_row	ed-span	
crates/aterm-grid/src/grid/tests/algorithm_boundary/history_extras.rs	cell_extra_mut_creates_and_updates_extra_data	has-extras-iff-entry	
crates/aterm-grid/src/grid/tests/algorithm_boundary/history_extras.rs	history_line_api_covers_tiered_scrollback_ordering	history-line-order	
crates/aterm-grid/src/grid/tests/algorithm_boundary/history_extras.rs	set_cell_complex_char_stores_overflow_string	has-extras-iff-entry	
crates/aterm-grid/src/grid/tests/algorithm_boundary/history_extras.rs	shift_region_down_boundary_rows	extras-shifts	
crates/aterm-grid/src/grid/tests/algorithm_boundary/history_extras.rs	shift_region_down_single_row_region	extras-shifts	
crates/aterm-grid/src/grid/tests/algorithm_boundary/history_extras.rs	shift_region_up_boundary_rows	extras-shifts	
crates/aterm-grid/src/grid/tests/algorithm_boundary/history_extras.rs	shift_region_up_single_row_region	extras-shifts	
crates/aterm-grid/src/grid/tests/algorithm_boundary/history_extras.rs	try_history_line_ring_buffer_path_is_infallible	history-line-order	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	back_tab_margin_preserves_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	back_tab_preserves_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	delete_lines_clears_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	delete_lines_outside_the_horizontal_margins_keeps_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	delete_lines_outside_the_scroll_region_keeps_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	insert_lines_clears_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	insert_lines_outside_the_horizontal_margins_keeps_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	insert_lines_outside_the_scroll_region_keeps_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	line_feed_always_clears_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	save_restore_cursor_preserves_pending_wrap	nonspec:delete	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	scroll_region_down_preserves_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	scroll_region_up_preserves_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	scroll_up_preserves_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	tab_preserves_pending_wrap	pending-wrap-motion-table	
crates/aterm-grid/src/grid/tests/algorithm_boundary/pending_wrap.rs	wrap_serial_counts_resolved_wraps_and_ignores_hard_newlines	autowrap-deferred	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	clamp_display_offset_limits_and_marks_damage_on_clamp_only	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	clamp_display_offset_small_delta_marks_bottom_rows_dirty	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	invariants_after_erase_display_modes	grid-type-invariant	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	invariants_after_line_feed_with_scroll_region	grid-type-invariant	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	invariants_after_reverse_line_feed_with_scroll_region	grid-type-invariant	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	invariants_resize_extreme_shrink_full_scrollback	grid-type-invariant	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	invariants_resize_reflow_while_scrolled_back	grid-type-invariant	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	invariants_resize_shrink_while_scrolled_back	grid-type-invariant	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	multipage_grid_resize_zeroes_cells	resize-clamp-and-reset	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	needs_full_redraw_distinguishes_partial_and_full_damage	content-damage-and-generation	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	reflow_grow_cursor_in_trailing_blank_of_continuation_row	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	reflow_grow_cursor_on_last_content_cell	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	reflow_shrink_cursor_at_chunk_boundary_exact	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	reflow_shrink_cursor_past_content_end	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	reflow_shrink_to_single_column	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	reset_scroll_region_restores_full_screen_region	region-and-margin-setters	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	resize_existing_grid_to_zero_both	resize-clamp-and-reset	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	resize_existing_grid_to_zero_cols	resize-clamp-and-reset	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	resize_existing_grid_to_zero_rows	resize-clamp-and-reset	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	resize_resets_cursor_state_after_geometry_change	resize-clamp-and-reset	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	row_index_after_resize_from_one_row	row-access-bounds	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	row_index_after_resize_to_one_row	row-access-bounds	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	row_index_one_row_grid_visible_row_zero	row-access-bounds	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	row_only_resize_invalidates_complex_char_ring	write-char-denotation	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	row_shrink_after_scrolling_preserves_newest_content	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	row_shrink_grow_cycle_nonzero_ring_head	resize-clamp-and-reset	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	row_shrink_with_scrollback_retains_history_within_the_ring_cap	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	visible_to_absolute_self_consistency_growth_phase	absolute-row-numbering	
crates/aterm-grid/src/grid/tests/algorithm_boundary/resize_and_invariants.rs	visible_to_absolute_self_consistency_reuse_phase	absolute-row-numbering	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scroll_display_damage.rs	content_scroll_delta_set_by_scroll_up_and_consumed	content-damage-and-generation	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scroll_display_damage.rs	scroll_display_clamped_no_damage	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scroll_display_damage.rs	scroll_display_down_marks_bottom_rows_dirty	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scroll_display_damage.rs	scroll_display_large_delta_marks_full_damage	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scroll_display_damage.rs	scroll_display_up_marks_top_rows_dirty	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scroll_display_damage.rs	scroll_display_zero_delta_no_damage	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	erase_to_end_of_screen_targeted_damage	content-damage-and-generation	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	line_feed_above_scroll_region_moves_down	line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	line_feed_after_scroll_display_resets_offset	line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	line_feed_at_bottom_margin_scrolls_region	line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	line_feed_below_region_clamped_at_screen_bottom	line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	line_feed_below_scroll_region_moves_down	line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	line_feed_full_screen_region_scrolls_with_scrollback	line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	line_feed_within_scroll_region_moves_down	line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	reverse_line_feed_above_region_clamped_at_row_zero	reverse-line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	reverse_line_feed_above_scroll_region_moves_up	reverse-line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	reverse_line_feed_after_scroll_display_resets_offset	reverse-line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	reverse_line_feed_at_top_margin_scrolls_region_down	reverse-line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	reverse_line_feed_within_scroll_region_moves_up	reverse-line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_degenerate_top_equals_bottom	region-and-margin-setters	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_down_margined_top_equals_bottom_is_noop	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_down_n_equals_region_size	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_down_normal_still_works	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_down_top_equals_bottom_is_noop	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_up_direct_with_nonzero_display_offset	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_up_margined_top_equals_bottom_is_noop	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_up_n_equals_region_size	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_up_n_exceeds_region_size	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_up_normal_still_works	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_up_targeted_damage	content-damage-and-generation	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_up_top_equals_bottom_is_noop	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_region_up_two_row	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_to_bottom_full_damage_large_offset	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_to_bottom_targeted_damage_small_offset	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_up_mixed_phase1_phase2_single_call	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	scroll_up_mixed_phase_scrollback_content	scroll-region-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	single_row_scroll_region_rejected_per_vt510	region-and-margin-setters	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	two_row_scroll_region_line_feed	line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	two_row_scroll_region_reverse_line_feed	reverse-line-feed	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	write_ascii_blast_at_scroll_region_bottom_scrolls_region_only	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	write_ascii_run_styled_at_scroll_region_bottom_scrolls_region_only	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	write_char_wrap_after_scroll_display_resets_offset	autowrap-deferred	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	write_char_wrap_at_scroll_region_bottom_scrolls_region_only	autowrap-deferred	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	write_char_wrap_below_scroll_region_at_screen_bottom_does_not_scroll	autowrap-deferred	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	write_char_wrap_styled_at_scroll_region_bottom_scrolls_region_only	autowrap-deferred	
crates/aterm-grid/src/grid/tests/algorithm_boundary/scrolling.rs	write_wide_char_wrap_at_scroll_region_bottom_scrolls_region_only	autowrap-deferred	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_cell_run.rs	write_cell_run_applies_style	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_cell_run.rs	write_cell_run_content_matches_per_char_write	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_cell_run.rs	write_cell_run_cursor_consistency_with_write_char_wrap	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_cell_run.rs	write_cell_run_empty_count	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_cell_run.rs	write_cell_run_fills_exact_line	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_cell_run.rs	write_cell_run_scrolls_at_screen_bottom	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_cell_run.rs	write_cell_run_wraps_across_lines	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	backspace_from_mid_column	cursor-horizontal-margins	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	carriage_return_at_col_zero_is_noop	carriage-return	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	carriage_return_then_line_feed_sequence	carriage-return	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	delete_chars_shifts_extras_left	ich-dch-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	erase_chars_at_content_boundary	ech-erase-chars	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	erase_chars_clears_extras	ech-erase-chars	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	erase_rect_inverted_coordinates_is_noop	rect-fill-erase	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	insert_chars_at_last_column	ich-dch-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	insert_chars_shifts_extras_right	ich-dch-shift	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	write_ascii_blast_at_cursor_offset	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	write_ascii_blast_content_matches_write_char	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	write_ascii_blast_empty_input	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	write_ascii_blast_fills_exact_line	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	write_ascii_blast_returns_correct_written_count	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	write_ascii_blast_scrolls_at_screen_bottom	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	write_ascii_blast_single_byte	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_ops.rs	write_ascii_blast_wraps_and_verifies_content_across_lines	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_styled.rs	write_ascii_run_styled_applies_colors	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_styled.rs	write_ascii_run_styled_applies_flags	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_styled.rs	write_ascii_run_styled_at_cursor_offset	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_styled.rs	write_ascii_run_styled_content_matches_write_char	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_styled.rs	write_ascii_run_styled_empty_input	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_styled.rs	write_ascii_run_styled_fills_exact_line_with_deferred_wrap	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_styled.rs	write_ascii_run_styled_last_byte_tracks_correctly	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_styled.rs	write_ascii_run_styled_scrolls_at_screen_bottom	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_styled.rs	write_ascii_run_styled_single_byte	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/algorithm_boundary/write_styled.rs	write_ascii_run_styled_wraps_across_lines	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/async_compress.rs	backpressure_bounds_the_backlog_under_sustained_overload	compress-offload-staging	
crates/aterm-grid/src/grid/tests/async_compress.rs	bounded_drain_is_a_noop_while_detached_for_reflow	compress-offload-staging	
crates/aterm-grid/src/grid/tests/async_compress.rs	bounded_drain_promotes_at_most_the_batch	compress-offload-staging	
crates/aterm-grid/src/grid/tests/async_compress.rs	deferred_backlog_stays_readable_then_drains_without_loss	compress-offload-staging	
crates/aterm-grid/src/grid/tests/async_compress.rs	inactive_offload_drains_inline_as_before	compress-offload-staging	
crates/aterm-grid/src/grid/tests/basic.rs	grid_assert_invariants_after_operations	grid-type-invariant	
crates/aterm-grid/src/grid/tests/basic.rs	grid_cursor_bounds	cursor-set-clamp	
crates/aterm-grid/src/grid/tests/basic.rs	grid_cursor_movement	cursor-set-clamp	
crates/aterm-grid/src/grid/tests/basic.rs	grid_cursor_movement_exact_amount	cursor-set-clamp	
crates/aterm-grid/src/grid/tests/basic.rs	grid_delete_chars	ich-dch-shift	
crates/aterm-grid/src/grid/tests/basic.rs	grid_delete_lines	il-dl	
crates/aterm-grid/src/grid/tests/basic.rs	grid_erase_line	el-span	
crates/aterm-grid/src/grid/tests/basic.rs	grid_insert_chars	ich-dch-shift	
crates/aterm-grid/src/grid/tests/basic.rs	grid_insert_lines	il-dl	
crates/aterm-grid/src/grid/tests/basic.rs	grid_new	grid-construct	
crates/aterm-grid/src/grid/tests/basic.rs	grid_resize	resize-clamp-and-reset	
crates/aterm-grid/src/grid/tests/basic.rs	grid_screen_alignment_pattern	decaln	
crates/aterm-grid/src/grid/tests/basic.rs	grid_scroll_up	line-feed	
crates/aterm-grid/src/grid/tests/basic.rs	grid_write_char	write-char-denotation	
crates/aterm-grid/src/grid/tests/basic.rs	grid_write_char_wrap	autowrap-deferred	
crates/aterm-grid/src/grid/tests/basic.rs	test_row_text_bmp_only_unchanged	write-char-denotation	
crates/aterm-grid/src/grid/tests/basic.rs	test_row_text_complex_chars_and_combining_marks	write-char-denotation	
crates/aterm-grid/src/grid/tests/basic.rs	write_char_bmp_not_affected_by_supplementary_fix	write-char-denotation	
crates/aterm-grid/src/grid/tests/basic.rs	write_char_preserves_supplementary_plane_emoji	write-char-denotation	
crates/aterm-grid/src/grid/tests/basic.rs	write_char_supplementary_mixed_with_ascii	write-char-denotation	
crates/aterm-grid/src/grid/tests/basic.rs	write_char_supplementary_sets_complex_flag	write-char-denotation	
crates/aterm-grid/src/grid/tests/basic.rs	write_char_wrap_marks_continuation_after_full_screen_scroll	autowrap-deferred	
crates/aterm-grid/src/grid/tests/basic.rs	write_char_wrap_preserves_supplementary_plane_emoji	write-char-denotation	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_cursor_at_max_position_then_resize_down	resize-clamp-and-reset	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_delete_chars_at_last_column	ich-dch-shift	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_delete_chars_zero_count	ich-dch-shift	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_delete_lines_exceeding_available	il-dl	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_erase_scrollback_empty	grid-type-invariant	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_history_line_api_covers_ring_buffer_ordering	history-line-order	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_insert_chars_at_col_zero	ich-dch-shift	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_insert_chars_zero_count	ich-dch-shift	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_insert_lines_exceeding_available	il-dl	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_resize_to_same_size_is_noop	resize-clamp-and-reset	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_scroll_down_zero	scroll-region-shift	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_scroll_up_zero	scroll-region-shift	
crates/aterm-grid/src/grid/tests/boundary.rs	grid_write_char_1x1_stays_at_origin	write-char-denotation	
crates/aterm-grid/src/grid/tests/boundary.rs	row_u16_identity_for_typical_values	row-u16-saturating	
crates/aterm-grid/src/grid/tests/boundary.rs	row_u16_saturates_on_overflow	row-u16-saturating	
crates/aterm-grid/src/grid/tests/boundary.rs	write_ascii_blast_and_char_wrap_cursor_consistency	run-writers-refine-write-char-wrap	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	cell_pool_is_bounded_by_depth_and_by_total_cells	lazy-buffer-pool-and-sums	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_ascii_row_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_combining_marks_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_complex_chars_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_empty_row_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_empty_row_wrapped_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_hyperlinks_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_mixed_content_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_multiple_hyperlinks_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_rgb_colors_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_styled_ascii_row_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_wide_chars_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	deferred_line_wrapped_row_equivalence	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	flood_backpressure_drop_recycles_bodies	lazy-buffer-pool-and-sums	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	front_rows_over_sheds_the_fewest_rows_that_fit_and_keeps_the_floor	lazy-buffer-pool-and-sums	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	lazy_buffer_drain_produces_equivalent_lines	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	lazy_buffer_recycles_bodies_across_a_drain	lazy-buffer-pool-and-sums	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	materialized_all_default_line_stores_no_attrs	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	materialized_default_prefix_then_styled_tail_keeps_both	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	materialized_multi_run_keeps_every_run_including_default_tail	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	materialized_single_nondefault_run_is_kept	deferred-line-equals-eager	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	recycled_cell_body_carries_no_state_from_the_previous_line	lazy-buffer-pool-and-sums	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	recycling_preserves_flooded_scrollback_content	lazy-buffer-pool-and-sums	
crates/aterm-grid/src/grid/tests/deferred_line_equivalence.rs	width_change_drops_recycled_bodies_and_history_survives	lazy-buffer-pool-and-sums	
crates/aterm-grid/src/grid/tests/extras_performance.rs	erase_rect_extras_clear_is_batch_o_e	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	erase_to_end_of_screen_extras_clear_is_batch_o_e	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	extras_clear_rows_is_batch_o_e	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	extras_shift_full_screen_is_o1_partial_is_o_e	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	rapid_scroll_extras_cost_bounded	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	reflow_grow_row_ops_linear_in_rows	reflow-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	reflow_shrink_row_ops_linear_in_rows	reflow-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	row_has_hyperlinks_after_mutations	extras-shifts	
crates/aterm-grid/src/grid/tests/extras_performance.rs	scroll_up_extras_cost_o1_regardless_of_e	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	scroll_up_extras_shift_is_o1	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	shift_cols_left_is_o_e_not_o_count	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	shift_cols_right_is_o_e_not_o_count	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	shift_region_down_by_is_o_e_not_o_region	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	shift_region_up_by_is_o_e_not_o_region	extras-op-cost	
crates/aterm-grid/src/grid/tests/extras_performance.rs	shift_rows_up_by_o1_regardless_of_entry_count	extras-op-cost	
crates/aterm-grid/src/grid/tests/history_link_budget.rs	a_shed_at_the_bottom_unlinks_the_rows_a_later_scroll_back_reads	history-link-budget	
crates/aterm-grid/src/grid/tests/history_link_budget.rs	a_shed_under_a_scrolled_back_viewport_redraws_the_rows_it_unlinks	history-link-budget	
crates/aterm-grid/src/grid/tests/history_link_budget.rs	an_offloaded_width_change_keeps_the_link_bound_through_its_reattach	history-link-budget	
crates/aterm-grid/src/grid/tests/history_link_budget.rs	ordinary_link_workloads_keep_every_link	history-link-budget	
crates/aterm-grid/src/grid/tests/history_link_budget.rs	ring_link_flood_is_held_to_the_budget_oldest_first	history-link-budget	
crates/aterm-grid/src/grid/tests/history_link_budget.rs	staged_link_flood_under_offload_is_held_to_the_budget	history-link-budget	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	dch_still_shifts_only_within_the_margins	ich-dch-shift	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	decdc_still_shifts_only_within_the_margins	decic-decdc	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	decic_still_shifts_only_within_the_margins	decic-decdc	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	decsel0_erases_past_the_right_margin	selective-erase-span	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	decsel1_erases_left_of_the_left_margin	selective-erase-span	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	decsel2_erases_the_whole_row_not_the_margin_span	selective-erase-span	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	ech_count_is_clipped_by_the_row_edge	ech-erase-chars	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	ech_erases_past_the_right_margin	ech-erase-chars	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	ed0_erases_the_cursor_row_past_the_right_margin	ed-span	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	ed1_erases_the_cursor_row_from_column_zero	ed-span	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	el0_erases_past_the_right_margin	el-span	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	el1_erases_left_of_the_left_margin	el-span	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	el2_erases_the_whole_row_not_the_margin_span	el-span	
crates/aterm-grid/src/grid/tests/horizontal_margin_scope.rs	ich_still_shifts_only_within_the_margins	ich-dch-shift	
crates/aterm-grid/src/grid/tests/hyperlink_perf.rs	correct_across_mixed_mutations	extras-shifts	
crates/aterm-grid/src/grid/tests/hyperlink_perf.rs	correct_after_clear_row	extras-shifts	
crates/aterm-grid/src/grid/tests/hyperlink_perf.rs	correct_after_clear_rows	extras-shifts	
crates/aterm-grid/src/grid/tests/hyperlink_perf.rs	correct_after_get_or_create	extras-shifts	
crates/aterm-grid/src/grid/tests/hyperlink_perf.rs	correct_after_set_empty	extras-shifts	
crates/aterm-grid/src/grid/tests/hyperlink_perf.rs	correct_after_shift_rows_up	extras-shifts	
crates/aterm-grid/src/grid/tests/link_translation_accessors.rs	base_y_equals_oldest_absolute_plus_scrollback	absolute-row-numbering	
crates/aterm-grid/src/grid/tests/link_translation_accessors.rs	display_origin_absolute_tracks_scroll	absolute-row-numbering	
crates/aterm-grid/src/grid/tests/link_translation_accessors.rs	row_is_wrapped_true_on_continuation_row	row-metadata-accessors	
crates/aterm-grid/src/grid/tests/link_translation_accessors.rs	row_len_is_logical_length	row-metadata-accessors	
crates/aterm-grid/src/grid/tests/link_translation_accessors.rs	scrolled_history_wrapped_row_metadata_is_tier_aware	row-metadata-accessors	
crates/aterm-grid/src/grid/tests/reflow/core.rs	reflow_cursor_at_chunk_boundary	reflow-cursor-tracking	
crates/aterm-grid/src/grid/tests/reflow/core.rs	reflow_cursor_at_wrapped_line_end	reflow-cursor-tracking	
crates/aterm-grid/src/grid/tests/reflow/core.rs	reflow_grow_preserves_cursor_position	reflow-cursor-tracking	
crates/aterm-grid/src/grid/tests/reflow/core.rs	reflow_grow_unwraps_soft_wrapped_lines	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/core.rs	reflow_handles_empty_rows	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/core.rs	reflow_preserves_hard_line_breaks	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/core.rs	reflow_round_trip	width-roundtrip-restores-topology	
crates/aterm-grid/src/grid/tests/reflow/core.rs	reflow_shrink_preserves_cursor_position	reflow-cursor-tracking	
crates/aterm-grid/src/grid/tests/reflow/core.rs	reflow_shrink_wraps_long_line	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/core.rs	reflow_without_reflow_flag	no-reflow-resize-truncates	
crates/aterm-grid/src/grid/tests/reflow/core.rs	resize_with_reflow_grow_preserves_nonempty_bottom_row_after_scroll	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/core.rs	resize_with_reflow_noop_and_grow_preserve_logical_rows_after_scroll	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	a_reflow_windows_cap_is_logged_at_its_first_drop_and_its_close	offload-window-cap-logging	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	abort_reclamps_the_viewport_when_the_staged_window_output_is_discarded	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	naive_width_resize_rewraps_whole_history_synchronously	width-resize-sync-cost	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_abort_drops_the_reader_aim	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_abort_recovers_grid_to_bounded_state	offload-window-conserves-history	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_preserves_deep_scroll_position	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_widen_keeps_the_scrolled_back_reader_in_history	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_width_change_keeps_an_unwrapped_reader_on_the_same_line	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_aim_moves_both_ways_and_home_is_a_choice	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_aim_rides_up_with_window_output	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_aim_top_and_the_restore	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_captures_concurrent_scrolloff_no_gap	offload-window-conserves-history	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_end_after_scrolling_over_window_output_is_honored	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_end_at_an_already_pinned_bottom_is_not_a_signal	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_erase_is_not_resurrected	offload-window-conserves-history	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_height_shrink_keeps_ring_scrollback	offload-window-conserves-history	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_lazy_buffer_is_bounded	offload-window-conserves-history	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_reader_who_moved_then_returned_to_live_chose_it	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_reader_who_only_scrolled_up_did_not_choose_the_bottom	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_scroll_to_bottom_not_clobbered	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offload_window_scroll_up_over_the_away_history_lands_at_the_reattach	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offloaded_resize_keeps_synchronous_reflow_within_the_ring	nonspec:delete	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	offloaded_resize_synchronous_rewrap_is_zero	width-resize-sync-cost	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	output_batch_pin_dance_is_not_a_reader_gesture	reader-live-bottom-gesture	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	redetach_clamps_the_viewport_and_still_restores_the_reader	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cost_bound.rs	redetach_window_end_after_scrolling_over_window_output_is_honored	offload-reader-position	
crates/aterm-grid/src/grid/tests/reflow/cursor_invariants.rs	cursor_invariant_exhaustive_small_grids	grid-type-invariant	
crates/aterm-grid/src/grid/tests/reflow/cursor_invariants.rs	cursor_invariant_multi_resize_cycle	grid-type-invariant	
crates/aterm-grid/src/grid/tests/reflow/cursor_invariants.rs	cursor_invariant_single_column_terminal	grid-type-invariant	
crates/aterm-grid/src/grid/tests/reflow/cursor_invariants.rs	cursor_invariant_single_row_terminal	grid-type-invariant	
crates/aterm-grid/src/grid/tests/reflow/cursor_invariants.rs	cursor_invariant_with_scrollback	grid-type-invariant	
crates/aterm-grid/src/grid/tests/reflow/cursor_invariants.rs	cursor_on_empty_row_grow	grid-type-invariant	
crates/aterm-grid/src/grid/tests/reflow/cursor_invariants.rs	cursor_on_empty_row_shrink	grid-type-invariant	
crates/aterm-grid/src/grid/tests/reflow/cursor_invariants.rs	cursor_on_last_row_with_expanding_content_shrink	grid-type-invariant	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdhl_pair_preserved_on_grow	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdhl_top_bottom_preserved_on_shrink	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdwl_continuation_row_not_merged_on_grow	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdwl_continuation_row_survives_shrink_merge	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdwl_cursor_clamped_on_shrink	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdwl_mixed_with_normal_rows_on_shrink	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdwl_round_trip_shrink_then_grow	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdwl_row_not_split_even_when_full	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdwl_row_preserved_on_grow	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdwl_row_preserved_on_shrink	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/decdwl.rs	decdwl_shrink_blanks_a_wide_char_cut_at_the_new_edge	double-size-rows-resize-in-place	
crates/aterm-grid/src/grid/tests/reflow/performance.rs	reflow_extreme_resize_preserves_cursor_on_last_row	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/performance.rs	reflow_grow_extreme_ratio_merges_continuations	reflow-cost	
crates/aterm-grid/src/grid/tests/reflow/performance.rs	reflow_shrink_extreme_ratio_is_linear_in_source_rows	reflow-cost	
crates/aterm-grid/src/grid/tests/reflow/performance.rs	reflow_shrink_preallocation_adequate	reflow-viewport-placement	
crates/aterm-grid/src/grid/tests/reflow/protected_cells.rs	resize_preserves_multiple_protected_cells_with_wide_chars	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/protected_cells.rs	resize_to_one_column_preserves_protected_cell_after_wide_char	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/protected_cells.rs	resize_to_one_column_preserves_protected_cells	no-reflow-resize-truncates	
crates/aterm-grid/src/grid/tests/reflow/protected_cells.rs	resize_to_one_column_preserves_protected_cells_with_reflow	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/protected_cells.rs	resize_to_one_column_skips_wide_char_spacer	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	adjust_row_count_height_decrease_no_scrollback_no_panic	grid-type-invariant	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	adjust_row_count_height_decrease_preserves_scrollback_content	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	column_reflow_preserves_ring_scrollback_both_directions	reflow-history-rewrap	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	column_reflow_round_trip_restores_scrollback_count	reflow-history-rewrap	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_extras_flags_consistent_after_grow	reflow-extras-follow-cells	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_extras_flags_consistent_after_shrink	reflow-extras-follow-cells	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_grow_preserves_underline_color	reflow-extras-follow-cells	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_round_trip_preserves_fg_rgb	width-roundtrip-restores-topology	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_row_shrink_retains_rows_and_extras_follow_regrow	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_shrink_drops_wide_char_extras_when_wide_cell_is_replaced	reflow-extras-follow-cells	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_shrink_hyperlink_moves_to_new_row	reflow-extras-follow-cells	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_shrink_multiple_extras_remapped	reflow-extras-follow-cells	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_shrink_preserves_bg_rgb	reflow-extras-follow-cells	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_shrink_preserves_complex_char_and_row_text	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	reflow_shrink_remaps_extras_within_first_chunk	reflow-extras-follow-cells	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	resize_reflow_with_nonzero_display_offset_preserves_live_content	resize-keeps-reader-position	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	width_reflow_tiered_history_precedes_visible_overflow	reflow-viewport-placement	
crates/aterm-grid/src/grid/tests/reflow/regressions.rs	width_reflow_tiered_restore_honors_line_limit	reflow-history-rewrap	
crates/aterm-grid/src/grid/tests/reflow/rgb_across_resize.rs	a_screen_with_no_truecolor_harvests_no_cell	rgb-harvest-cost	
crates/aterm-grid/src/grid/tests/reflow/rgb_across_resize.rs	a_second_resize_harvests_nothing	rgb-harvest-cost	
crates/aterm-grid/src/grid/tests/reflow/rgb_across_resize.rs	alt_quiet_flap_keeps_truecolor	resize-undo-flap-identity	
crates/aterm-grid/src/grid/tests/reflow/rgb_across_resize.rs	alt_width_change_keeps_truecolor	truecolor-survives-resize	
crates/aterm-grid/src/grid/tests/reflow/rgb_across_resize.rs	main_width_change_keeps_truecolor	truecolor-survives-resize	
crates/aterm-grid/src/grid/tests/reflow/rgb_across_resize.rs	rows_only_resize_keeps_truecolor_fg_bg	truecolor-survives-resize	
crates/aterm-grid/src/grid/tests/reflow/rgb_across_resize.rs	same_size_resize_keeps_truecolor	truecolor-survives-resize	
crates/aterm-grid/src/grid/tests/reflow/rgb_across_resize.rs	two_resizes_keep_truecolor	truecolor-survives-resize	
crates/aterm-grid/src/grid/tests/reflow/rows_only_cost_bound.rs	rows_only_grow_migrates_nothing	rows-only-resize-cost	
crates/aterm-grid/src/grid/tests/reflow/rows_only_cost_bound.rs	rows_only_resize_keeps_tiered_ring_history_extras	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_cost_bound.rs	rows_only_resize_leaves_history_in_the_ring	rows-only-resize-cost	
crates/aterm-grid/src/grid/tests/reflow/rows_only_cost_bound.rs	rows_only_resize_migration_is_bounded_by_the_height_delta	rows-only-resize-cost	
crates/aterm-grid/src/grid/tests/reflow/rows_only_cost_bound.rs	rows_only_resize_migration_is_independent_of_ring_depth	rows-only-resize-cost	
crates/aterm-grid/src/grid/tests/reflow/rows_only_cost_bound.rs	width_resize_still_reflows_and_preserves_history	reflow-history-rewrap	
crates/aterm-grid/src/grid/tests/reflow/rows_only_identity.rs	a_rows_only_resize_keeps_every_absolute_key	absolute-row-numbering	
crates/aterm-grid/src/grid/tests/reflow/rows_only_identity.rs	cursor_near_top_shrink_preserves_all_content	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_identity.rs	extras_survive_the_round_trip_through_ring_history	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_identity.rs	full_screen_shrink_grow_round_trip_is_identity	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_identity.rs	history_reading_order_survives_a_round_trip	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_identity.rs	prompt_with_trailing_blanks_does_not_walk_across_toggles	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_ring_retention.rs	line_feeds_after_a_wrapped_ring_grows_in_rows_scroll_every_row	scroll-up-appends-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_ring_retention.rs	rows_only_grow_cursor_follows_its_content_line	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_ring_retention.rs	rows_only_grow_reveals_history_and_matches_tiered	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_ring_retention.rs	rows_only_resize_keeps_ring_history_extras	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_ring_retention.rs	rows_only_shrink_at_cap_evicts_only_beyond_the_limit	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_ring_retention.rs	rows_only_shrink_preserves_ring_history_and_matches_tiered	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_ring_retention.rs	rows_only_shrink_with_zero_cap_still_retains_nothing	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/reflow/rows_only_ring_retention.rs	top_anchored_region_scrolls_after_a_wrapped_ring_grows_in_rows	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/reflow/wide_chars.rs	reflow_grow_cursor_at_end_of_wide_char_content	reflow-cursor-tracking	
crates/aterm-grid/src/grid/tests/reflow/wide_chars.rs	reflow_narrow_terminal_replaces_multiple_wide_chars	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/wide_chars.rs	reflow_narrow_terminal_skips_wide_spacer_only_after_replacement	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/wide_chars.rs	reflow_narrow_terminal_with_wide_chars_clamps_cursor	reflow-cursor-tracking	
crates/aterm-grid/src/grid/tests/reflow/wide_chars.rs	reflow_narrow_terminal_with_wide_chars_terminates	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/wide_chars.rs	reflow_shrink_wide_char_at_chunk_boundary	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/wide_chars.rs	resize_no_reflow_clears_wide_char_at_boundary	no-reflow-resize-truncates	
crates/aterm-grid/src/grid/tests/reflow/wide_chars.rs	resize_without_reflow_clears_unfittable_wide_chars	no-reflow-resize-truncates	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	clear_mid_flight_does_not_resurrect_history_offloaded	reflow-viewport-placement	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	early_wrap_pad_preserves_real_trailing_spaces	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	early_wrapped_wide_char_survives_sweep_across_history_boundary	width-roundtrip-restores-topology	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	grow_merges_early_wrapped_wide_char_without_phantom_space	width-roundtrip-restores-topology	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	offloaded_resize_while_scrolled_back_keeps_erased_band_blank	reflow-viewport-placement	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	offloaded_resize_while_scrolled_back_matches_unscrolled_topology	width-roundtrip-restores-topology	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	width_sweep_keeps_prompt_anchored_to_content	reflow-viewport-placement	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	width_sweep_preserves_spaces_at_chunk_boundaries	reflow-rewrap-layout	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	width_sweep_round_trip_restores_wrap_topology_offloaded	width-roundtrip-restores-topology	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	width_sweep_round_trip_restores_wrap_topology_ring	width-roundtrip-restores-topology	
crates/aterm-grid/src/grid/tests/reflow/width_sweep.rs	width_sweep_round_trip_restores_wrap_topology_tiered	width-roundtrip-restores-topology	
crates/aterm-grid/src/grid/tests/resize_row_reuse.rs	repeated_resize_shapes_and_intervening_output_keep_the_allocation_highwater	resize-row-reuse-refines-model	
crates/aterm-grid/src/grid/tests/resize_row_reuse.rs	repeated_rows_only_resize_reuses_page_allocations	resize-row-reuse-refines-model	
crates/aterm-grid/src/grid/tests/resize_row_reuse.rs	resize_reveal_then_output_reuses_the_retained_ring_highwater	resize-row-reuse-refines-model	
crates/aterm-grid/src/grid/tests/resize_row_reuse.rs	resize_row_reuse_conforms	resize-row-reuse-refines-model	
crates/aterm-grid/src/grid/tests/scroll_damage.rs	clamp_display_offset_small_delta_marks_bottom_rows	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/scroll_damage.rs	content_gen_advances_on_content_mutations	content-gen-tracks-content	
crates/aterm-grid/src/grid/tests/scroll_damage.rs	content_gen_starts_nonzero	content-gen-tracks-content	
crates/aterm-grid/src/grid/tests/scroll_damage.rs	content_gen_tracks_row_text_changes	content-gen-tracks-content	
crates/aterm-grid/src/grid/tests/scroll_damage.rs	content_gen_unchanged_on_noop_scroll_up	content-gen-tracks-content	
crates/aterm-grid/src/grid/tests/scroll_damage.rs	content_gen_unchanged_on_viewport_scroll	content-gen-tracks-content	
crates/aterm-grid/src/grid/tests/scroll_damage.rs	scroll_to_absolute_row_places_target_at_top_and_clamps	display-offset-navigation	
crates/aterm-grid/src/grid/tests/scroll_damage.rs	scroll_to_top_near_top_uses_partial_damage	viewport-scroll-damage	
crates/aterm-grid/src/grid/tests/scroll_damage.rs	top_visible_absolute_row_tracks_the_display_offset	display-offset-navigation	
crates/aterm-grid/src/grid/tests/scroll_region/band_moves.rs	a_codex_phase_a_line_records_the_viewport_band_and_no_splice	row-band-moves-denote-ops	
crates/aterm-grid/src/grid/tests/scroll_region/band_moves.rs	a_reverse_index_at_a_region_top_records_one_composed_down_band	row-band-moves-denote-ops	
crates/aterm-grid/src/grid/tests/scroll_region/band_moves.rs	a_top_anchored_archival_scroll_records_the_transcript_band_beside_its_splice	row-band-moves-denote-ops	
crates/aterm-grid/src/grid/tests/scroll_region/band_moves.rs	band_moves_compose_exactly_and_refuse_everything_else	row-band-move-compose	
crates/aterm-grid/src/grid/tests/scroll_region/band_moves.rs	margined_scrolls_line_ops_and_forced_invalidations_poison_the_band_record	row-band-moves-denote-ops	
crates/aterm-grid/src/grid/tests/scroll_region/hyperlink_regressions.rs	grid_delete_lines_shifts_hyperlinks	il-dl	
crates/aterm-grid/src/grid/tests/scroll_region/hyperlink_regressions.rs	grid_insert_lines_shifts_hyperlinks	il-dl	
crates/aterm-grid/src/grid/tests/scroll_region/hyperlink_regressions.rs	grid_scroll_region_down_shifts_hyperlinks	scroll-region-shift	
crates/aterm-grid/src/grid/tests/scroll_region/hyperlink_regressions.rs	grid_scroll_region_up_shifts_hyperlinks	scroll-region-shift	
crates/aterm-grid/src/grid/tests/scroll_region/hyperlink_regressions.rs	grid_top_anchored_partial_scroll_preserves_history_and_footer_hyperlinks	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_delete_lines_cursor_outside_scroll_region	il-dl	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_delete_lines_margined_full_width_falls_back	il-dl-margined	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_delete_lines_margined_rectangular_shift	il-dl-margined	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_delete_lines_resets_nonzero_display_offset	il-dl	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_delete_lines_with_scroll_region	il-dl	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_fresh_session_mixed_batch_scroll_drops_only_leading_blank_prefix	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_history_free_top_region_remains_ephemeral	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_insert_lines_cursor_outside_scroll_region	il-dl	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_insert_lines_margined_count_exceeds_region	il-dl-margined	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_insert_lines_margined_cursor_outside_noop	il-dl-margined	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_insert_lines_margined_lrmm_off_falls_back	il-dl-margined	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_insert_lines_margined_rectangular_shift	il-dl-margined	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_insert_lines_resets_nonzero_display_offset	il-dl	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_insert_lines_with_scroll_region	il-dl	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_repeated_top_anchored_scrolls_coalesce_footer_row_splices	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_scroll_region_down_resets_nonzero_display_offset	scroll-region-shift	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_scroll_region_down_within_region	scroll-region-shift	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_scroll_region_up_resets_nonzero_display_offset	scroll-region-shift	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_scroll_region_up_within_region	scroll-region-shift	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_top_anchored_partial_batch_scroll_archives_each_displaced_row	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_top_anchored_partial_scroll_archives_history_and_preserves_footer	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_top_anchored_partial_scroll_clamps_to_region_size	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_top_anchored_partial_scroll_keeps_history_order_at_ring_capacity	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_top_anchored_partial_scroll_preserves_dense_extras_and_bce	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_top_anchored_scroll_archives_interior_blank_between_written_lines	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_top_anchored_scroll_archives_written_then_erased_row	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_top_anchored_scroll_of_never_written_rows_mints_no_history	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/region_ops.rs	grid_zero_ring_tiered_history_archives_top_anchored_partial_scroll	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_aborts_on_decompression_failure	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_all_empties_scrollback	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_combining_mark_not_split	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_from_scrollback_basic	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_limited_by_available_scrollback	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_no_scrollback_fallback	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_preserves_attributes	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_preserves_extended_visual_flags	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_preserves_wrapped_flag	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_removes_lines_from_scrollback	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_restores_hyperlinks	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_restores_newest_ring_lines_in_order	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_ring_only_history_is_not_a_blank_scroll	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_spanning_ring_and_store_keeps_age_order	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_wide_char_at_last_col_dropped	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_wide_char_column_alignment	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_with_scroll_region	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_zero_does_nothing	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll.rs	grid_unscroll_zwj_emoji_not_split	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll_region_scroll.rs	grid_unscroll_clamps_region_scroll_delta_to_available_scrollback	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll_region_scroll.rs	grid_unscroll_full_screen_marks_all_rows_dirty	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scroll_region/unscroll_region_scroll.rs	grid_unscroll_scroll_region_marks_only_region_rows_dirty	unscroll-denotation	
crates/aterm-grid/src/grid/tests/scrollback.rs	a_full_screen_scroll_past_the_screen_archives_one_screenful	top-anchored-region-archive	
crates/aterm-grid/src/grid/tests/scrollback.rs	a_prefilled_tiered_store_is_numbered_below_the_live_top	absolute-row-numbering	
crates/aterm-grid/src/grid/tests/scrollback.rs	erase_scrollback_at_ring_buffer_capacity	ed3-erase-scrollback	
crates/aterm-grid/src/grid/tests/scrollback.rs	erase_scrollback_single_line	ed3-erase-scrollback	
crates/aterm-grid/src/grid/tests/scrollback.rs	flood_backpressure_drops_are_counted_and_marked	flood-history-no-silent-hole	
crates/aterm-grid/src/grid/tests/scrollback.rs	grid_attach_detach_scrollback	nonspec:delete	
crates/aterm-grid/src/grid/tests/scrollback.rs	grid_erase_scrollback_preserves_live_rows	ed3-erase-scrollback	
crates/aterm-grid/src/grid/tests/scrollback.rs	grid_scroll_display	display-offset-navigation	
crates/aterm-grid/src/grid/tests/scrollback.rs	grid_scrollback_content_preserved	scroll-up-appends-transcript	
crates/aterm-grid/src/grid/tests/scrollback.rs	grid_scrollback_wrapped_lines	scroll-up-appends-transcript	
crates/aterm-grid/src/grid/tests/scrollback.rs	grid_with_tiered_scrollback	scroll-up-appends-transcript	
crates/aterm-grid/src/grid/tests/scrollback.rs	height_decrease_preserves_extras_on_pushed_rows	rows-only-resize-transcript	
crates/aterm-grid/src/grid/tests/scrollback.rs	history_lines_from_matches_get_history_line_across_every_source	history-walk-refines-get	
crates/aterm-grid/src/grid/tests/scrollback.rs	history_lines_from_matches_get_history_line_over_a_corrupt_segment	history-walk-refines-get	
crates/aterm-grid/src/grid/tests/scrollback.rs	retention_shrink_bumps_content_gen_only_when_lines_evict	content-gen-tracks-content	
crates/aterm-grid/src/grid/tests/scrollback.rs	ring_buffer_scrollback_preserves_hyperlinks	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback.rs	ring_extras_len_equals_ring_buffer_scrollback	grid-type-invariant	
crates/aterm-grid/src/grid/tests/scrollback.rs	ring_only_limit_grow_extends_retention_lazily	retention-limit	
crates/aterm-grid/src/grid/tests/scrollback.rs	ring_only_limit_none_is_unlimited	retention-limit	
crates/aterm-grid/src/grid/tests/scrollback.rs	ring_only_limit_reflects_in_the_getter	retention-limit	
crates/aterm-grid/src/grid/tests/scrollback.rs	ring_only_limit_shrink_clamps_scrolled_back_viewport	retention-limit	
crates/aterm-grid/src/grid/tests/scrollback.rs	ring_only_limit_shrink_evicts_oldest_immediately	retention-limit	
crates/aterm-grid/src/grid/tests/scrollback.rs	ring_watermark_reports_pressure_only_when_configured	ring-watermark-level	
crates/aterm-grid/src/grid/tests/scrollback.rs	scroll_up_n_exceeds_capacity	scroll-up-appends-transcript	
crates/aterm-grid/src/grid/tests/scrollback.rs	scroll_up_spans_growth_and_reuse_phases	scroll-up-appends-transcript	
crates/aterm-grid/src/grid/tests/scrollback.rs	scrollback_preserves_complex_chars_rgb_and_combining	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback.rs	staged_lazy_buffer_bytes_count_toward_memory_used	grid-memory-accounting	
crates/aterm-grid/src/grid/tests/scrollback.rs	tiered_limit_below_ring_cap_recaps_the_ring	retention-limit	
crates/aterm-grid/src/grid/tests/scrollback.rs	tiered_limit_equal_to_ring_cap_discards_evictions_without_store_churn	retention-limit	
crates/aterm-grid/src/grid/tests/scrollback.rs	tiered_limit_is_one_total_across_ring_and_store	retention-limit	
crates/aterm-grid/src/grid/tests/scrollback.rs	tiered_limit_none_is_unlimited_total	retention-limit	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	advance_grapheme_unit_bounds_pathological_zwj_chain	grapheme-unit-segmentation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	advance_grapheme_unit_consecutive_combining_marks	grapheme-unit-segmentation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	advance_grapheme_unit_folds_a_flag_half_onto_its_base_past_a_mark	grapheme-unit-segmentation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	advance_grapheme_unit_orphan_zwj_mid_text	grapheme-unit-segmentation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	advance_grapheme_unit_single_ascii	grapheme-unit-segmentation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	advance_grapheme_unit_variation_selector	grapheme-unit-segmentation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	advance_grapheme_unit_wide_skin_tone_fold_gates_on_effective_width	grapheme-unit-segmentation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	advance_grapheme_unit_wide_vs15_narrows_wide_emoji	grapheme-unit-segmentation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	advance_grapheme_unit_wide_vs16_widens_narrow_emoji	grapheme-unit-segmentation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	advance_grapheme_unit_zwj_emoji_sequence	grapheme-unit-segmentation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	materialize_consecutive_combining_marks	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	materialize_orphan_zwj_between_visible_chars	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_grapheme_edge.rs	materialize_stops_when_wide_char_cannot_fit_at_last_column	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_images.rs	a_grid_that_never_drew_a_picture_never_scans_for_one	extract-row-extras-spans	
crates/aterm-grid/src/grid/tests/scrollback_images.rs	a_run_broken_by_a_second_placement_splits_into_two_spans	extract-row-extras-spans	
crates/aterm-grid/src/grid/tests/scrollback_images.rs	a_scrolled_off_image_row_round_trips_through_a_line_back_to_per_cell_refs	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_images.rs	an_image_only_row_yields_one_span_per_footprint_row_when_it_scrolls_off	extract-row-extras-spans	
crates/aterm-grid/src/grid/tests/scrollback_images.rs	an_image_row_still_materializes_as_empty_text	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_images.rs	the_ring_fast_materializer_agrees_with_the_line_round_trip_on_an_image_row	line-consumers-agree	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	extract_row_extras_hyperlink_spans_use_physical_columns	extract-row-extras-spans	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	fill_row_from_line_preserves_hyperlink_id	line-consumers-agree	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	fill_row_mixed_content_allocation_free_path	line-consumers-agree	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	hyperlink_id_survives_full_grid_scrollback_grid_roundtrip	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	line_roundtrip_carries_rendition_extras_into_the_wide_spacer	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	line_roundtrip_carries_rendition_into_the_wide_spacer	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	line_roundtrip_preserves_write_time_ambiguous_width	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_drops_spacer_but_keeps_protected_neighbor	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_combining_mark	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_empty_line	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_end_to_end_styled_scrollback	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_mixed_content	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_preserves_hyperlink_id	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_restores_emoji	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_restores_hyperlinks	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_restores_rgb_colors	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_rgb_background	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_roundtrip_ascii	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_roundtrip_wide_chars	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_truncation	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_from_line_zwj_family_emoji	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_keeps_a_vs16_cell_stored_narrow_narrow_in_mid_row	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_keeps_decsca_protected_text	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_keeps_unfolded_skin_tone_split	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_keeps_vs16_heart_narrow_at_last_column	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_mixed_content_allocation_free_path	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_preserves_vs15_narrowed_cell	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_preserves_vs16_widened_cell	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	materialize_wide_char_at_last_col_does_not_corrupt_rgb	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	scrollback_hyperlink_wide_char_round_trip	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize.rs	wide_spacer_mirror_preserves_an_indexed_underline_colour	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize_len.rs	materialized_row_len_ascii_content	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize_len.rs	materialized_row_len_combining_mark	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize_len.rs	materialized_row_len_complex_emoji	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize_len.rs	materialized_row_len_empty_row	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize_len.rs	materialized_row_len_matches_row_len_for_content	row-line-roundtrip	
crates/aterm-grid/src/grid/tests/scrollback_materialize_len.rs	materialized_row_len_styled_trailing_spaces	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize_len.rs	materialized_row_len_trailing_spaces_after_content	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_materialize_len.rs	materialized_row_len_wide_cjk	materialize-from-line-denotation	
crates/aterm-grid/src/grid/tests/scrollback_style_roundtrip.rs	row_to_line_rgb_missing_extras_returns_default_colors	row-to-line-colour-resolution	
crates/aterm-grid/src/grid/tests/scrollback_style_roundtrip.rs	style_id_cells_missing_style_fallback_to_defaults	row-to-line-colour-resolution	
crates/aterm-grid/src/grid/tests/scrollback_style_roundtrip.rs	style_id_cells_preserve_colors_in_scrollback_roundtrip	row-to-line-colour-resolution	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	an_extras_run_never_adopts_an_orphaned_entry	writes-replace-extras	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	emoji_run_autowrap_tightens_len_when_the_run_orphans_the_row_tail	nonspec:delete	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	failed_wide_write_does_not_drop_live_extras	nonspec:delete	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	wide_run_autowrap_clears_orphaned_wide_half_at_run_start	writes-replace-extras	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	wide_run_autowrap_tightens_len_when_the_run_orphans_the_row_tail	writes-replace-extras	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	write_ascii_blast_clears_stale_extras_entries	writes-replace-extras	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	write_ascii_blast_preserves_extras_outside_written_range	writes-replace-extras	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	write_ascii_run_styled_clears_stale_extras_entries	nonspec:delete	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	write_ascii_run_with_extras_does_not_merge_stale_hyperlink	writes-replace-extras	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	write_cell_run_clears_stale_extras_entries	writes-replace-extras	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	write_char_clears_stale_extras_entry	writes-replace-extras	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	write_narrow_autowrap_fast_clears_stale_extras_entry	writes-replace-extras	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	write_wide_autowrap_fast_clears_stale_extras_on_both_halves	writes-replace-extras	
crates/aterm-grid/src/grid/tests/stale_extras_overwrite.rs	write_wide_run_autowrap_clears_stale_extras_entries	writes-replace-extras	
crates/aterm-grid/src/grid/tests/style_perf.rs	clear_hyperlink_clears_has_extras_when_empty	has-extras-iff-entry	
crates/aterm-grid/src/grid/tests/style_perf.rs	colored_prompt_has_extras_only_on_prompt_cells	has-extras-iff-entry	
crates/aterm-grid/src/grid/tests/style_perf.rs	erase_to_end_of_screen_clears_has_extras	has-extras-iff-entry	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_one_row_invariants_valid	grid-type-invariant	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_scroll_down_full_screen_clears_all	scroll-region-shift	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_scroll_down_shifts_content_and_clears_top	scroll-region-shift	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_scroll_region_bottom_strictly_less_than_visible_rows	grid-type-invariant	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_scroll_region_down_clears_top_rows	scroll-region-shift	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_style_id_deduplication_in_writes	nonspec:delete	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_write_char_with_style_id_default	nonspec:delete	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_write_char_with_style_id_extra_flags	nonspec:delete	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_write_char_with_style_id_indexed_color	nonspec:delete	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_write_char_with_style_id_rgb_color	nonspec:delete	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_write_char_wrap_with_style_id	nonspec:delete	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_write_wide_char_with_style_id	nonspec:delete	
crates/aterm-grid/src/grid/tests/style_perf.rs	grid_write_wide_char_wrap_with_style_id	nonspec:delete	
crates/aterm-grid/src/grid/tests/style_perf.rs	mark_cursor_damage_after_cursor_move	mark-cursor-damage	
crates/aterm-grid/src/grid/tests/style_perf.rs	mark_cursor_damage_marks_cursor_cell	mark-cursor-damage	
crates/aterm-grid/src/grid/tests/style_perf.rs	remove_cell_extra_clears_flag_and_entry	has-extras-iff-entry	
crates/aterm-grid/src/grid/tests/style_perf.rs	remove_cell_extra_preserves_cell_content	has-extras-iff-entry	
crates/aterm-grid/src/grid/tests/style_perf.rs	row_access_at_ring_tiered_boundary	row-access-bounds	
crates/aterm-grid/src/grid/tests/style_perf.rs	row_returns_none_when_scrolled_into_tiered_scrollback	row-access-bounds	
crates/aterm-grid/src/grid/tests/style_perf.rs	sync_extras_flags_for_row_clears_stale_flags	has-extras-iff-entry	
crates/aterm-grid/src/grid/tests/style_perf.rs	sync_extras_flags_for_row_sets_flags_correctly	has-extras-iff-entry	
crates/aterm-grid/src/grid/tests/style_perf_complexity.rs	reflow_linear_time	nonspec:becomes-spec-after-fix	
crates/aterm-grid/src/grid/tests/style_perf_complexity.rs	reflow_linear_time_columns	nonspec:becomes-spec-after-fix	
crates/aterm-grid/src/grid/tests/style_perf_complexity.rs	row_to_line_extracts_hyperlinks	extract-row-extras-spans	
crates/aterm-grid/src/grid/tests/style_perf_complexity.rs	row_to_line_linear_in_columns	scroll-off-is-lazy	
crates/aterm-grid/src/grid/tests/style_perf_complexity.rs	row_to_line_no_hyperlinks	extract-row-extras-spans	
crates/aterm-grid/src/grid/tests/style_perf_complexity.rs	scroll_single_line_constant_time	scroll-off-is-lazy	
crates/aterm-grid/src/grid/tests/style_perf_complexity.rs	scroll_up_handles_many_rows	scroll-off-is-lazy	
crates/aterm-grid/src/grid/tests/style_perf_complexity.rs	scroll_up_linear_time	scroll-off-is-lazy	
crates/aterm-grid/src/grid/tests/tab_stops.rs	cbt_clamps_to_the_left_margin_under_origin_mode	cbt-prev-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	cbt_ignores_the_left_margin_without_origin_mode	cbt-prev-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	cbt_left_of_the_left_margin_never_moves_the_cursor	cbt-prev-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	cbt_without_declrmm_is_the_plain_back_tab_in_either_mode	cbt-prev-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	cht_right_of_the_right_margin_never_moves_the_cursor_backward	ht-next-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_back_tab_between_stops	cbt-prev-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_back_tab_n	cbt-prev-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_back_tab_with_custom_stops	cbt-prev-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_back_tab_with_default_stops	cbt-prev-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_clear_all_tab_stops	tab-stop-set-clear	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_clear_tab_stop	tab-stop-set-clear	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_default_tab_stops	tab-default-stops	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_is_tab_stop	nonspec:delete	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_reset_tab_stops	tab-default-stops	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_resize_preserves_tab_stops	tab-stops-resize-grow-only	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_set_tab_stop	tab-stop-set-clear	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_tab_n_from_between_stops	ht-next-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_tab_n_past_last_stop	ht-next-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_tab_n_with_custom_stops	ht-next-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_tab_n_with_default_stops	ht-next-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_tab_stop_positions	nonspec:delete	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_tab_uses_custom_stops	ht-next-stop	
crates/aterm-grid/src/grid/tests/tab_stops.rs	grid_widen_still_seeds_defaults_when_no_one_cleared_them	tab-stops-resize-grow-only	
crates/aterm-grid/src/grid/tests/tab_stops.rs	restore_preserves_bounded_stops_beyond_narrow_width_for_later_grow	restore-tab-stops-admission	
crates/aterm-grid/src/grid/tests/zero_store_share.rs	zero_share_drag_step_during_the_offloaded_rewrap_keeps_the_rings_history	zero-share-tiered-is-ring-only	
crates/aterm-grid/src/grid/tests/zero_store_share.rs	zero_share_grid_matches_its_ring_only_twin_through_resizes	zero-share-tiered-is-ring-only	
crates/aterm-grid/src/grid/tests/zero_store_share.rs	zero_share_replacement_store_in_an_offload_window_stages_nothing	history-contiguous-suffix	
crates/aterm-grid/src/grid/tests/zero_store_share.rs	zero_share_rows_shrinking_rewrap_free_width_change_reads_as_its_twin	zero-share-tiered-is-ring-only	
crates/aterm-grid/src/grid/tests/zero_store_share.rs	zero_share_width_shrink_keeps_history_a_contiguous_suffix	history-contiguous-suffix	
crates/aterm-grid/src/grid/visible_row_view.rs	a_content_change_invalidates_every_memoized_row	viewport-memo-sharing	
crates/aterm-grid/src/grid/visible_row_view.rs	a_line_arriving_while_scrolled_back_reshuffles_rows_correctly	viewport-memo-transparent	
crates/aterm-grid/src/grid/visible_row_view.rs	a_scrolled_row_keeps_its_materialization_at_its_new_viewport_index	viewport-memo-sharing	
crates/aterm-grid/src/grid/visible_row_view.rs	an_unscrolled_viewport_reads_live_rows	viewport-memo-transparent	
crates/aterm-grid/src/grid/visible_row_view.rs	memoized_rows_match_a_fresh_materialize_across_a_scrub	viewport-memo-transparent	
crates/aterm-grid/src/grid/visible_row_view.rs	repeat_read_of_a_motionless_viewport_reuses_the_materialized_row	viewport-memo-sharing	
crates/aterm-grid/src/grid/write_split.rs	wide_wrap_tail_paints_the_bisected_orphan_with_the_bce_blank_and_its_ring	wide-autowrap-tail-bce	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	alloc_slice_alignment_u128	alloc-slice-contract	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	alloc_slice_alignment_u64	alloc-slice-contract	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	alloc_slice_alignment_u8	alloc-slice-contract	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	alloc_slice_cell_alignment_and_access	alloc-slice-contract	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	alloc_slice_pointer_stability_across_allocations	page-store-row-separation	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	alloc_slice_pointer_stability_across_pages	page-store-row-separation	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	alloc_slice_sequential_no_overlap	alloc-slice-contract	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	alloc_slice_write_read_roundtrip_after_reset	nonspec:delete	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	alloc_zero_length_slice_is_stable_and_non_consuming	alloc-slice-contract	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	offset_near_boundary_u32	nonspec:delete	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	offset_out_of_bounds_returns_none	nonspec:delete	
crates/aterm-grid/src/page_tests/pointer_correctness.rs	reset_three_pages_all_zeroed_on_reuse	nonspec:delete	
crates/aterm-grid/src/page_tests/store_ops.rs	alloc_new_page_when_full	page-pool-reuse-and-memory	
crates/aterm-grid/src/page_tests/store_ops.rs	alloc_uses_free_list	page-pool-reuse-and-memory	
crates/aterm-grid/src/page_tests/store_ops.rs	lazy_zeroing_handles_multiple_pages	nonspec:delete	
crates/aterm-grid/src/page_tests/store_ops.rs	lazy_zeroing_only_zeros_used_portion	nonspec:delete	
crates/aterm-grid/src/page_tests/store_ops.rs	offset_roundtrip	nonspec:delete	
crates/aterm-grid/src/page_tests/store_ops.rs	page_is_zeroed	alloc-slice-contract	
crates/aterm-grid/src/page_tests/store_ops.rs	page_store_allocates_slices	alloc-slice-contract	
crates/aterm-grid/src/page_tests/store_ops.rs	page_used_bytes_tracked_correctly	nonspec:delete	
crates/aterm-grid/src/page_tests/store_ops.rs	preheat_allocates_pages	page-pool-reuse-and-memory	
crates/aterm-grid/src/page_tests/store_ops.rs	recycled_row_cells_are_cleared_unique_and_width_matched	recycled-row-cells	
crates/aterm-grid/src/page_tests/store_ops.rs	reset_moves_pages_to_free_list	nonspec:delete	
crates/aterm-grid/src/page_tests/store_ops.rs	resetting_pages_discards_recycled_handles_before_page_reuse	nonspec:delete	
crates/aterm-grid/src/page_tests/store_ops.rs	reused_pages_are_zeroed	nonspec:delete	
crates/aterm-grid/src/page_tests/store_ops.rs	shrink_to_fit_releases_free_pages	page-pool-reuse-and-memory	
crates/aterm-grid/src/page_tests/store_ops.rs	stats_tracking_accurate	nonspec:delete	
crates/aterm-grid/src/page_tests/store_ops.rs	total_memory_tracks_all_pages	page-pool-reuse-and-memory	
crates/aterm-grid/src/page_tests/store_ops.rs	with_capacity_preheats	page-pool-reuse-and-memory	
crates/aterm-grid/src/page_tests/thread_safety.rs	allocate_reset_cross_page_zeroed	nonspec:delete	
crates/aterm-grid/src/page_tests/thread_safety.rs	allocate_reset_cycle_always_zeroed	nonspec:delete	
crates/aterm-grid/src/page_tests/thread_safety.rs	guard_page_send_sync	nonspec:delete	
crates/aterm-grid/src/page_tests/thread_safety.rs	guard_page_slice_send_sync	nonspec:delete	
crates/aterm-grid/src/page_tests/thread_safety.rs	lazy_zeroing_small_then_large_reallocation	nonspec:delete	
crates/aterm-grid/src/page_tests/thread_safety.rs	offset_get_bounds_and_alignment	nonspec:delete	
crates/aterm-grid/src/page_tests/thread_safety.rs	offset_get_mut_non_overlapping_offsets	nonspec:delete	
crates/aterm-grid/src/page_tests/thread_safety.rs	offset_get_mut_roundtrip	nonspec:delete	
crates/aterm-grid/src/page_tests/thread_safety.rs	offset_get_mut_then_get_same_offset	nonspec:delete	
crates/aterm-grid/src/page_tests/thread_safety.rs	page_send_across_thread	page-send-sync-sound	
crates/aterm-grid/src/page_tests/thread_safety.rs	page_slice_send_across_thread	page-send-sync-sound	
crates/aterm-grid/src/page_tests/thread_safety.rs	page_slice_sync_concurrent_readers	page-send-sync-sound	
crates/aterm-grid/src/page_tests/thread_safety.rs	page_sync_concurrent_readers	page-send-sync-sound	
crates/aterm-grid/src/pin.rs	test_evict_all_increments_all_pages	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_all_invalidates_all_pages	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_all_then_recreate_pins_are_valid	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_all_updates_min_valid_generation	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_page_auto_grows_capacity	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_page_does_not_affect_other_pages	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_page_increments_global_generation	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_page_increments_page_generation	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_page_multiple_times	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_pages_from_beyond_capacity_is_noop	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_pages_from_evicts_all_from_start	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_evict_pages_from_increments_global_once	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_generation_tracker_ensure_capacity_grows	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_generation_tracker_ensure_capacity_grows_larger	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_generation_tracker_ensure_capacity_idempotent	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_generation_tracker_new_starts_at_zero	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_generation_tracker_page_generation_unknown_page_is_zero	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_is_potentially_valid_after_eviction_returns_false	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_is_potentially_valid_allows_future_generation	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_is_potentially_valid_below_min_valid_returns_false	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_is_potentially_valid_fresh_pin	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_is_valid_after_evicting_different_page	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_is_valid_after_eviction_returns_false	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_is_valid_fresh_pin	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_is_valid_pin_with_future_generation	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_is_valid_recreated_pin_after_eviction	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_mixed_evict_page_and_evict_all	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pin_from_absolute_large_row_roundtrip	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pin_from_absolute_max_u32_row	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pin_from_absolute_small_row	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pin_from_absolute_zero	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pin_new_stores_all_fields	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pin_with_col_updates_column_only	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pin_with_row_offset_updates_row_only	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_is_valid_both_invalid	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_is_valid_both_pins_valid	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_is_valid_end_invalid	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_is_valid_start_invalid	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_normalized_already_ordered	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_normalized_is_idempotent	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_normalized_preserves_generation	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_normalized_same_row_different_col	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_normalized_same_row_ordered_col	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_normalized_same_row_same_col	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_pinned_range_normalized_swaps_reversed	nonspec:delete	
crates/aterm-grid/src/pin.rs	test_sequential_evictions_accumulate	nonspec:delete	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	clear_from_zero_sets_len_to_zero	nonspec:delete	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	clear_range_middle_preserves_len	clear-range-with-orphan	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	delete_chars_at_col_zero	dch-delete-chars-fill	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	delete_chars_at_last_col	dch-delete-chars-fill	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	delete_chars_removes_continuation_orphans_wide	dch-delete-chars-fill	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	delete_chars_splits_wide_char_at_boundary	dch-delete-chars-fill	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	erase_chars_middle_preserves_len	ech-erase-chars-with	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	erase_chars_wide_head_at_tail_recalcs_len	ech-erase-chars-with	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	insert_chars_at_col_zero	ich-insert-chars-fill	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	insert_chars_at_wide_continuation_clears_orphan	ich-insert-chars-fill	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	insert_chars_count_exceeds_remaining_space	ich-insert-chars-fill	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	row_set_last_valid_column_updates_len	row-set	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	row_set_page_max_cols_last_valid	row-set	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	row_set_repeated_same_column_no_len_regression	row-set	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	row_set_single_column_row	row-set	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	selective_clear_all_protected_preserves_len	selective-erase-decsca	
crates/aterm-grid/src/row/tests/algorithm_audit.rs	selective_clear_range_len_with_protected_at_end	selective-erase-decsca	
crates/aterm-grid/src/row/tests/basic_ops.rs	fill_cell_run_basic	fill-cell-run	
crates/aterm-grid/src/row/tests/basic_ops.rs	fill_cell_run_clamps_to_row_end	fill-cell-run	
crates/aterm-grid/src/row/tests/basic_ops.rs	fill_cell_run_full_row	fill-cell-run	
crates/aterm-grid/src/row/tests/basic_ops.rs	fill_cell_run_out_of_bounds_col	fill-cell-run	
crates/aterm-grid/src/row/tests/basic_ops.rs	fill_cell_run_overwrites_wide_chars	fill-cell-run	
crates/aterm-grid/src/row/tests/basic_ops.rs	fill_cell_run_with_style	fill-cell-run	
crates/aterm-grid/src/row/tests/basic_ops.rs	fill_cell_run_zero_count	fill-cell-run	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_clear	row-clear-erase-reset	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_clear_from	nonspec:delete	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_clear_from_sparse_tail	nonspec:delete	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_delete_chars	dch-delete-chars-fill	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_delete_chars_more_than_remaining	dch-delete-chars-fill	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_delete_chars_tail_overlap	dch-delete-chars-fill	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_erase_chars	ech-erase-chars-with	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_erase_chars_beyond_end	ech-erase-chars-with	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_erase_chars_zero_count	ech-erase-chars-with	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_insert_chars	ich-insert-chars-fill	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_insert_chars_at_end	ich-insert-chars-fill	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_insert_chars_truncation_drops_tail	ich-insert-chars-fill	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_new	row-new	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_resize_grow	row-resize	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_resize_shrink	row-resize	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_to_string	row-display	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_wrapped_flag	row-wrapped-flag	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_write_char	row-narrow-write	
crates/aterm-grid/src/row/tests/basic_ops.rs	row_write_overwrite_wide_continuation	row-narrow-write	
crates/aterm-grid/src/row/tests/bit10_protected.rs	bounded_ich_dch_preserve_protected_cell_outside_the_margin	bit10-locality	
crates/aterm-grid/src/row/tests/bit10_protected.rs	dch_preserves_left_neighbor	bit10-locality	
crates/aterm-grid/src/row/tests/bit10_protected.rs	dch_preserves_protected_cell_shifting_into_the_gap	bit10-locality	
crates/aterm-grid/src/row/tests/bit10_protected.rs	ech_preserves_left_neighbor	bit10-locality	
crates/aterm-grid/src/row/tests/bit10_protected.rs	fixup_wide_boundary_preserves_protected_visible_cells	fixup-wide-boundary	
crates/aterm-grid/src/row/tests/bit10_protected.rs	fixup_wide_boundary_suppressed_preserves_protected_spaces	fixup-wide-boundary	
crates/aterm-grid/src/row/tests/bit10_protected.rs	fixup_wide_boundary_suppressed_still_clears_dangling_wide_heads	fixup-wide-boundary	
crates/aterm-grid/src/row/tests/bit10_protected.rs	ich_preserves_left_neighbor_and_shifts_protected_cell	bit10-locality	
crates/aterm-grid/src/row/tests/bit10_protected.rs	write_over_a_genuine_wide_continuation_still_clears_the_orphaned_head	row-narrow-write	
crates/aterm-grid/src/row/tests/bit10_protected.rs	write_paths_do_not_destroy_left_neighbor_when_overwriting_a_protected_cell	bit10-locality	
crates/aterm-grid/src/row/tests/operations.rs	clear_range_clamps_end_to_cols	clear-range-with-orphan	
crates/aterm-grid/src/row/tests/operations.rs	clear_range_no_op_when_start_equals_end	clear-range-with-orphan	
crates/aterm-grid/src/row/tests/operations.rs	copy_from_same_width	row-copy-from	
crates/aterm-grid/src/row/tests/operations.rs	copy_from_wider_source_clamps	row-copy-from	
crates/aterm-grid/src/row/tests/operations.rs	line_size_default_is_single_width	row-line-size	
crates/aterm-grid/src/row/tests/operations.rs	resize_same_size_is_noop	row-resize	
crates/aterm-grid/src/row/tests/operations.rs	resize_wide_char_at_boundary_is_cleared	row-resize	
crates/aterm-grid/src/row/tests/operations.rs	selective_clear_range_empty_range	selective-erase-decsca	
crates/aterm-grid/src/row/tests/operations.rs	selective_clear_range_skips_protected_cells	selective-erase-decsca	
crates/aterm-grid/src/row/tests/operations.rs	selective_clear_skips_protected_cells	selective-erase-decsca	
crates/aterm-grid/src/row/tests/operations.rs	set_line_size_back_to_single_width	row-line-size	
crates/aterm-grid/src/row/tests/operations.rs	set_line_size_double_height_bottom	row-line-size	
crates/aterm-grid/src/row/tests/operations.rs	set_line_size_double_height_top	row-line-size	
crates/aterm-grid/src/row/tests/operations.rs	set_line_size_double_width	row-line-size	
crates/aterm-grid/src/row/tests/operations.rs	update_len_clamps_to_cols	cells-mut-with-fixup	
crates/aterm-grid/src/row/tests/operations.rs	update_len_extends_but_never_shrinks	cells-mut-with-fixup	
crates/aterm-grid/src/row/tests/style_id.rs	row_write_char_with_style_id_basic	nonspec:delete	
crates/aterm-grid/src/row/tests/style_id.rs	row_write_char_with_style_id_out_of_bounds	nonspec:delete	
crates/aterm-grid/src/row/tests/style_id.rs	row_write_char_with_style_id_overwrites_wide_continuation	nonspec:delete	
crates/aterm-grid/src/row/tests/style_id.rs	row_write_char_with_style_id_preserves_flags	nonspec:delete	
crates/aterm-grid/src/row/tests/style_id.rs	row_write_wide_char_with_style_id_at_edge	nonspec:delete	
crates/aterm-grid/src/row/tests/style_id.rs	row_write_wide_char_with_style_id_basic	nonspec:delete	
crates/aterm-grid/src/row/tests/style_id.rs	row_write_wide_char_with_style_id_overwrites_existing_wide	nonspec:delete	
crates/aterm-grid/src/row/tests/style_id.rs	row_write_wide_char_with_style_id_preserves_flags	nonspec:delete	
crates/aterm-grid/src/row/tests/style_id.rs	write_char_with_style_id_shrinks_len_on_tail_wide_head_overwrite	nonspec:delete	
crates/aterm-grid/src/row/tests/style_id.rs	write_wide_char_with_style_id_shrinks_len_on_tail_wide_orphan	nonspec:delete	
crates/aterm-grid/src/row/tests/unsafe_boundary/boundaries.rs	write_char_styled_all_columns_sequential	row-narrow-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/boundaries.rs	write_char_styled_first_col	row-narrow-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/boundaries.rs	write_char_styled_last_col	row-narrow-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/boundaries.rs	write_char_styled_out_of_bounds	row-narrow-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/boundaries.rs	write_char_styled_single_col_row	row-narrow-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/boundaries.rs	write_wide_char_first_col	row-wide-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/boundaries.rs	write_wide_char_last_col_rejected	row-wide-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/boundaries.rs	write_wide_char_last_valid_col	row-wide-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/boundaries.rs	write_wide_char_single_col_row_rejected	row-wide-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/boundaries.rs	write_wide_char_two_col_row	row-wide-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/memory_safety.rs	multiple_rows_independent_writes	page-store-row-separation	
crates/aterm-grid/src/row/tests/unsafe_boundary/memory_safety.rs	resize_preserves_content_across_reallocation	row-resize	
crates/aterm-grid/src/row/tests/unsafe_boundary/memory_safety.rs	write_wide_char_every_valid_position	row-wide-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/overwrite.rs	write_char_styled_overwrite_wide_first_half	row-narrow-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/overwrite.rs	write_char_styled_overwrite_wide_second_half	row-narrow-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/overwrite.rs	write_wide_char_adjacent_pair	row-wide-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/overwrite.rs	write_wide_char_overwrite_existing_wide_overlap	row-wide-write	
crates/aterm-grid/src/row/tests/unsafe_boundary/unchecked.rs	get_unchecked_first_and_last_col	row-get-unchecked	
crates/aterm-grid/src/row/tests/unsafe_boundary/unchecked.rs	get_unchecked_mut_out_of_bounds_panics_in_debug	nonspec:delete	
crates/aterm-grid/src/row/tests/unsafe_boundary/unchecked.rs	get_unchecked_mut_write_and_read	row-get-unchecked	
crates/aterm-grid/src/row/tests/unsafe_boundary/unchecked.rs	get_unchecked_out_of_bounds_panics_in_debug	nonspec:delete	
crates/aterm-grid/src/row/tests/unsafe_boundary/unchecked.rs	get_unchecked_valid_read	row-get-unchecked	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	an_early_wrap_hole_ends_when_its_cell_is_rewritten_with_a_blank	early-wrap-hole-lifecycle	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	an_early_wrap_hole_lasts_until_its_cell_is_written	early-wrap-hole-lifecycle	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	cells_mut_with_fixup_allows_zero_count	cells-mut-with-fixup	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	cells_mut_with_fixup_clears_orphaned_continuation	cells-mut-with-fixup	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	cells_mut_with_fixup_clears_wide_continuation_past_range	cells-mut-with-fixup	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	cells_mut_with_fixup_handles_multiple_wide_chars	cells-mut-with-fixup	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	cells_mut_with_fixup_ignores_out_of_bounds_start_without_overflow	cells-mut-with-fixup	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	cells_mut_with_fixup_is_noop_without_wide_chars	cells-mut-with-fixup	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	cells_mut_with_fixup_returns_none_when_start_past_end	cells-mut-with-fixup	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	cells_mut_with_fixup_shrinks_len_on_tail_wide_orphan	cells-mut-with-fixup	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	clear_range_shrinks_len_on_tail_wide_orphan	clear-range-with-orphan	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	clear_range_with_does_not_bleed_fill_onto_left_wide_orphan	clear-range-with-orphan	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	clear_range_with_does_not_bleed_fill_onto_right_wide_orphan	clear-range-with-orphan	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	clear_range_with_empty_fill_shrinks_len_on_tail_wide_orphan	clear-range-with-orphan	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	clear_range_with_keeps_rect_orphans_empty_under_a_visible_fill	clear-range-with-orphan	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	clear_range_with_orphan_paints_left_wide_orphan_with_the_bce_blank	clear-range-with-orphan	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	clear_range_with_orphan_paints_right_wide_orphan_with_the_bce_blank	clear-range-with-orphan	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	delete_chars_fill_shrinks_len_on_deleted_wide_head_at_tail	dch-delete-chars-fill	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	fill_cell_run_shrinks_len_on_tail_wide_orphan	fill-cell-run	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	fixup_wide_boundary_clears_orphaned_continuation_at_left	fixup-wide-boundary	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	insert_chars_bounded_fill_clears_phantom_continuation	bounded-ich-dch	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	selective_clear_range_shrinks_len_on_tail_wide_orphan	selective-erase-decsca	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	selective_erase_chars_clears_left_boundary_continuation	nonspec:delete	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	selective_erase_chars_shrinks_len_on_tail_wide_orphan	nonspec:delete	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	selective_wipe_range_shrinks_len_on_tail_wide_orphan	selective-erase-decsca	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	style_id_wide_write_sets_has_wide_chars_for_later_narrow_overwrite	nonspec:delete	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	write_char_packed_shrinks_len_on_tail_wide_head_overwrite	row-narrow-write	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	write_char_shrinks_len_on_tail_wide_head_overwrite	row-narrow-write	
crates/aterm-grid/src/row/tests/wide_char_fixup.rs	write_wide_char_packed_shrinks_len_on_tail_wide_orphan	row-wide-write	
crates/aterm-grid/src/row/tests/wide_spacer_rendition.rs	wide_continuation_of_a_bare_lead_is_the_bare_role_bit	wide-continuation-of	
crates/aterm-grid/src/row/tests/wide_spacer_rendition.rs	wide_continuation_of_keeps_every_rendition_and_only_its_own_role_bit	wide-continuation-of	
crates/aterm-grid/src/row/tests/wide_spacer_rendition.rs	write_wide_char_inverse_reaches_the_spacer	row-wide-write	
crates/aterm-grid/src/row/tests/wide_spacer_rendition.rs	write_wide_char_no_fixup_matches_the_checked_path	row-wide-write	
crates/aterm-grid/src/row/tests/wide_spacer_rendition.rs	write_wide_char_rules_reach_the_spacer	row-wide-write	
crates/aterm-grid/src/row/tests/wide_spacer_rendition.rs	write_wide_char_with_style_id_inverse_reaches_the_spacer	nonspec:delete	
crates/aterm-grid/src/row/write.rs	fill_cell_run_basic_fill	fill-cell-run	
crates/aterm-grid/src/row/write.rs	fill_cell_run_counts_written_blanks	fill-cell-run	
crates/aterm-grid/src/row/write.rs	fill_cell_run_overwrites_wide_at_end_boundary	fill-cell-run	
crates/aterm-grid/src/row/write.rs	fill_cell_run_overwrites_wide_at_start_boundary	fill-cell-run	
crates/aterm-grid/src/row/write.rs	fixup_wide_char_overwrite_clears_continuation	orphan-fixup-primitives	
crates/aterm-grid/src/row/write.rs	fixup_wide_char_overwrite_clears_main_when_at_continuation	orphan-fixup-primitives	
crates/aterm-grid/src/row/write.rs	fixup_wide_char_write_handles_overlapping_wides	orphan-fixup-primitives	
crates/aterm-grid/src/row/write.rs	write_char_ascii_multiple_sequential	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_ascii_single	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_at_last_column	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_bmp_accented	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_bmp_cjk_ideograph_single_cell	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_bmp_greek	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_extends_len_for_sparse_writes	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_out_of_bounds_returns_false	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_overwrite_wide_continuation_clears_main	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_overwrite_wide_first_half_clears_continuation	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_overwrites_existing	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_packed_ascii	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_packed_out_of_bounds	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_packed_overwrite_wide_continuation_clears_main	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_packed_overwrite_wide_main_clears_continuation	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_packed_updates_len	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_sets_dirty_flag	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_char_styled_basic	row-narrow-write	
crates/aterm-grid/src/row/write.rs	write_wide_char_len_accounts_for_both_cells	row-wide-write	
crates/aterm-grid/src/row/write.rs	write_wide_char_overwrite_existing_wide_orphan_cleanup	row-wide-write	
crates/aterm-grid/src/row/write.rs	write_wide_char_packed_basic	row-wide-write	
crates/aterm-grid/src/row/write.rs	write_wide_char_packed_out_of_bounds	row-wide-write	
crates/aterm-grid/src/row/write.rs	write_wide_char_sets_has_wide_chars_flag	row-wide-write	
crates/aterm-grid/src/state/presentation.rs	adjacent_and_overlapping_bands_coalesce	selection-damage-union-lattice	
crates/aterm-grid/src/state/presentation.rs	disjoint_bands_stay_disjoint	selection-damage-union-lattice	
crates/aterm-grid/src/state/presentation.rs	none_is_the_unit_and_all_absorbs	selection-damage-union-lattice	
crates/aterm-grid/src/state/presentation.rs	overflow_absorbs_a_pair_and_still_covers_every_damaged_row	selection-damage-overflow-absorb	
crates/aterm-grid/src/state/presentation.rs	past_the_arity_bound_one_gap_is_given_up_not_all_of_them	selection-damage-overflow-absorb	
crates/aterm-grid/src/state/presentation.rs	union_is_order_independent	selection-damage-union-lattice	
crates/aterm-grid/src/state/presentation.rs	union_is_order_independent_past_the_arity_bound	selection-damage-union-lattice	
crates/aterm-grid/src/style_color.rs	grid_ansi_table_matches_color_palette	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_build_compaction_map_dead_maps_to_default	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_build_compaction_map_does_not_mutate	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_build_compaction_map_live_count	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_clear_leaves_only_default	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_clear_preserves_lookup_for_default	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_clear_resets_l1_cache	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_extended_default_id	style-table-new-holds-only-default	
crates/aterm-grid/src/style_table.rs	test_extended_invalid_id_returns_none	style-table-new-holds-only-default	
crates/aterm-grid/src/style_table.rs	test_extended_preserves_color_type_info	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_get_interned_style	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_get_invalid_id_returns_none	style-table-new-holds-only-default	
crates/aterm-grid/src/style_table.rs	test_indexed_l2_cache_hit	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_insert_new_style_returns_sequential_ids	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_intern_after_clear	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_intern_default_style_increments_default_refcount	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_intern_default_style_returns_default_id	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_intern_extended_dedup_no_extra_entry	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_intern_extended_twice_refcount_is_2	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_intern_many_distinct_styles	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_intern_style_both_indexed	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_intern_style_indexed_fg	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_intern_style_rgb_bg	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_intern_style_with_attrs	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_release_at_zero_does_not_underflow	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_release_out_of_bounds_is_safe	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_rgb_l2_cache_hit	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_try_intern_l1_hit_after_intern_extended	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_try_intern_l1_increments_refcount	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_try_intern_l1_miss_for_different_style	nonspec:delete	
crates/aterm-grid/src/style_table.rs	test_try_intern_l1_miss_on_empty_cache	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	all_style_attrs_roundtrip	attrs-to-cell-flags-table	
crates/aterm-grid/src/style_tests.rs	build_compaction_map_all_live_is_identity	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	build_compaction_map_is_dense_and_read_only	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	color_constructors	color-new-to-rgb	
crates/aterm-grid/src/style_tests.rs	color_from_rgb_tuple	color-new-to-rgb	
crates/aterm-grid/src/style_tests.rs	color_is_default	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	extended_style_from_cell_style_default	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	extended_style_from_cell_style_indexed	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	extended_style_from_cell_style_rgb	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	extended_style_roundtrip	attrs-to-cell-flags-table	
crates/aterm-grid/src/style_tests.rs	intern_different_styles	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	intern_extended_style	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	intern_same_style	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	style_constructors	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	style_id_default	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	style_intern_constant_time	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	style_is_default	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	style_setters	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	style_table_cap_at_u16_max	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	style_table_saturation_preserves_existing_styles	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	style_table_saturation_with_dead_styles_degrades_to_default	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	superscript_and_subscript_roundtrip_between_cell_flags_and_style_attrs	attrs-to-cell-flags-table	
crates/aterm-grid/src/style_tests.rs	table_clear	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	table_default_style	style-table-new-holds-only-default	
crates/aterm-grid/src/style_tests.rs	table_get_id	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	table_release	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	table_release_default_not_decremented	nonspec:delete	
crates/aterm-grid/src/style_tests.rs	table_stats	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	default_style_is_empty_and_cached_as_default	current-style-cache-invariant	
crates/aterm-grid/src/terminal_style.rs	one_attribute_or_colour_leaves_the_default	current-style-cache-invariant	
crates/aterm-grid/src/terminal_style.rs	test_basic_flags_no_style_extras	current-style-cache-invariant	
crates/aterm-grid/src/terminal_style.rs	test_bg_packed_roundtrip	current-style-new-fields	
crates/aterm-grid/src/terminal_style.rs	test_both_rgb_colors	current-style-cache-invariant	
crates/aterm-grid/src/terminal_style.rs	test_build_extended_style_bg_changed	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_build_extended_style_both_changed	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_build_extended_style_fg_changed	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_build_extended_style_indexed_bg	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_build_extended_style_rgb_fg	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_build_style_default	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_build_style_with_bold	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_build_style_with_indexed_fg	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_clear_bold_flag	current-style-cache-invariant	
crates/aterm-grid/src/terminal_style.rs	test_curly_underline	current-style-cache-invariant	
crates/aterm-grid/src/terminal_style.rs	test_dashed_underline	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_dotted_underline	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_double_underline	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_fg_packed_roundtrip	current-style-new-fields	
crates/aterm-grid/src/terminal_style.rs	test_flags_bits_roundtrip	current-style-new-fields	
crates/aterm-grid/src/terminal_style.rs	test_multiple_flags_combined	current-style-cache-invariant	
crates/aterm-grid/src/terminal_style.rs	test_new_with_all_fields	current-style-cache-invariant	
crates/aterm-grid/src/terminal_style.rs	test_protected_prevents_is_default	current-style-cache-invariant	
crates/aterm-grid/src/terminal_style.rs	test_reset_clears_all	current-style-reset	
crates/aterm-grid/src/terminal_style.rs	test_reset_sgr_preserves_protected	current-style-reset	
crates/aterm-grid/src/terminal_style.rs	test_reset_sgr_without_protected_is_default	current-style-reset	
crates/aterm-grid/src/terminal_style.rs	test_same_style_new_vs_default	current-style-new-fields	
crates/aterm-grid/src/terminal_style.rs	test_saved_cursor_state_default	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_saved_cursor_state_preserves_style	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_single_underline	nonspec:delete	
crates/aterm-grid/src/terminal_style.rs	test_subscript_triggers_style_extras	current-style-cache-invariant	
crates/aterm-grid/src/terminal_style.rs	test_superscript_triggers_style_extras	current-style-cache-invariant	
crates/aterm-grid/tests/conformance_offload.rs	offload_window_spec_model_checked_in_trust	offload-window-refinement	
crates/aterm-grid/tests/conformance_offload.rs	real_grid_detached_settings_conform_to_spec	offload-window-refinement	
crates/aterm-grid/tests/conformance_offload.rs	real_grid_offload_window_conforms_to_spec_exhaustively	offload-window-refinement	
crates/aterm-grid/tests/conformance_offload.rs	replacement_backed_abort_preserves_window_output_in_spec	offload-window-refinement	
crates/aterm-grid/tests/conformance_retention.rs	real_grid_retention_conforms_to_spec	unified-retention-limit-refinement	
crates/aterm-grid/tests/conformance_retention.rs	resurrected_pre_unification_split_is_rejected	unified-retention-limit-refinement	
crates/aterm-grid/tests/conformance_retention.rs	retention_limit_spec_model_checked_in_trust	unified-retention-limit-refinement	
crates/aterm-grid/tests/conformance_top_anchored_scroll.rs	real_grid_top_anchored_scroll_regimes_conform_to_derived_model	top-anchored-scroll-refinement	
crates/aterm-grid/tests/grid_miri.rs	miri_grid_resize_reflow	row-unsafe-ub-free	reflow-rewrap-layout
crates/aterm-grid/tests/grid_miri.rs	miri_grid_resize_stress	row-unsafe-ub-free	reflow-rewrap-layout
crates/aterm-grid/tests/grid_miri.rs	miri_many_rows_page_boundary	page-store-row-separation	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_multi_row_no_aliasing	page-store-row-separation	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_page_store_alloc_reuse	row-new	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_page_store_preheat_reuse	row-new	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_row_cells_mut_bulk_write	nonspec:delete	
crates/aterm-grid/tests/grid_miri.rs	miri_row_copy_from	row-copy-from	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_row_new_write_read	row-narrow-write	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_row_resize_grow	row-resize	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_row_resize_sequence	row-resize	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_row_resize_shrink	row-resize	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_row_wide_char_overwrite_fixup	row-narrow-write	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_row_wide_char_overwrite_second_half	row-narrow-write	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_row_write_char_styled	row-narrow-write	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_row_write_wide_char	row-wide-write	row-unsafe-ub-free
crates/aterm-grid/tests/grid_miri.rs	miri_row_write_wide_char_at_edge	row-wide-write	row-unsafe-ub-free
crates/aterm-grid/tests/grid_unsafe_access_properties.rs	grid_row_storage_does_not_alias_across_rows	page-store-row-separation	
crates/aterm-grid/tests/grid_unsafe_access_properties.rs	row_get_unchecked_matches_safe_access	row-get-unchecked	
crates/aterm-grid/tests/grid_unsafe_access_properties.rs	row_get_unchecked_mut_updates_only_target_cell	row-get-unchecked	
crates/aterm-grid/tests/history_renumber_epoch.rs	a_width_reflow_advances_the_history_renumber_epoch_and_a_rows_only_resize_does_not	history-renumber-epoch-on-width-reflow	
crates/aterm-grid/tests/history_renumber_epoch.rs	a_width_reflow_with_no_scrollback_still_advances_the_renumber_epoch	history-renumber-epoch-on-width-reflow	
crates/aterm-grid/tests/security_regression_5573.rs	multiple_rows_cross_isolation	page-store-row-separation	
crates/aterm-grid/tests/security_regression_5573.rs	resize_one_row_doesnt_corrupt_others	page-store-row-separation	
crates/aterm-grid/tests/security_regression_5573.rs	row_new_and_resize_stay_unsafe_fns	nonspec:delete	
crates/aterm-grid/tests/security_regression_5573.rs	row_new_valid_unsafe_usage	row-new	
crates/aterm-grid/tests/security_regression_5573.rs	row_resize_shrink_valid_unsafe_usage	row-resize	
crates/aterm-grid/tests/security_regression_5573.rs	row_resize_valid_unsafe_usage	row-resize	
```

## 3. Coverage

- 1805 tests read. 1561 map to one of the 206 specs (16 of them also sample a second spec). 244 are not spec samples (§4).
- The biggest collapses (primary tests): `extras-shifts` (64), `damage-tracker-model` (36), `il-dl` (35), `row-narrow-write` (34), `scroll-region-shift` (34), `ich-dch-shift` (30), `reflow-rewrap-layout` (29), `run-writers-refine-write-char-wrap` (28), `grid-cell-text-reads` (28), `row-line-roundtrip` (27).
- Proving `chunk-cells-to-rows`, `adjust-chunk-boundary` and `cursor-in-chunk` gives the chunking lemma that `reflow-rewrap-layout`, `reflow-cursor-tracking`, `reflow-history-rewrap` and `width-roundtrip-restores-topology` (51 primary tests between them) reuse; `row-narrow-write`/`row-wide-write` and the `orphan-fixup-primitives` are the lemmas of every grid write, erase and line-op spec.
- 206 specs remain after merging the same property seen from different files across the six passes, and after dropping two specs whose code is `#[cfg(test)]` (`generation-tracker-evict`, `style-intern-resolve`; §5.1).

## 4. Tests that are not spec samples

**Status (2026-10-15): deleted.** Every test named below was deleted (224 tests), except these 20, which were not in the confirmed deletion batch and remain in the tree: a_row_replaced_by_a_fresh_allocation_is_seen_by_both_readings, cursor_save_and_restore, failed_wide_write_does_not_drop_live_extras, generation_tracker_evict, generation_tracker_new, get_unchecked_mut_out_of_bounds_panics_in_debug, get_unchecked_out_of_bounds_panics_in_debug, grid_style_id_deduplication_in_writes, grid_write_char_with_style_id_default, grid_write_char_with_style_id_extra_flags, grid_write_char_with_style_id_indexed_color, grid_write_char_with_style_id_rgb_color, plain_run_equivalence_rejects_lost_text_attributes_and_wrap, reflow_linear_time, reflow_linear_time_columns, reflow_step_timing, row_new_and_resize_stay_unsafe_fns, save_restore_cursor_preserves_pending_wrap, test_shift_region_out_of_region_extras_skip_drain_rebuild, write_ascii_run_styled_clears_stale_extras_entries. The #[cfg(test)] helpers left dead by the deletions were removed with them (`impl Pin`, `impl PinnedRange`, `Cell::{set_style_id,clear_style_id,clear}`, `Offset::{get,get_mut}`, `Row::clear_from`, among others).

| tests | count | why | action |
|---|---|---|---|
| `src/cell_constructors.rs` (test_with_overflow_index_sets_complex_flag, test_with_overflow_index_stores_index, test_with_overflow_index_max_value, test_with_overflow_index_default_colors) | 4 | Tests the #[cfg(test)] constructor Cell::with_overflow_index. cell-mutators specifies the production path set_overflow_index, and cell-read-accessors specifies char() of a complex cell. | **delete** |
| `src/cell_constructors.rs` (test_with_style_id_sets_uses_style_id_flag, test_with_style_id_stores_style_id_in_colors, test_with_style_id_preserves_character, test_with_style_id_non_bmp_stores_replacement, test_with_style_id_merges_cell_flags, test_with_style_id_max_style_id, test_from_ascii_with_style_id_stores_byte, test_from_ascii_with_style_id_sets_style, test_from_ascii_with_style_id_merges_flags); `src/cell_tests.rs` (cell_with_style_id_default, cell_with_style_id_non_default, cell_with_style_id_preserves_cell_flags, cell_from_ascii_with_style_id, cell_style_id_opt_when_using_style, cell_style_id_opt_when_using_inline_colors, cell_set_style_id, cell_clear_style_id, cell_style_id_max_value, cell_with_style_id_wide_continuation) | 19 | Tests #[cfg(test)]-only StyleId cell code (with_style_id, from_ascii_with_style_id, set_style_id, clear_style_id, style_id_opt). Production never sets USES_STYLE_ID (terminal_style.rs:41); cell-color-accessors and cell-flags-bit-algebra cover the production readers. | **delete** |
| `src/cell_tests.rs` (cell_clear, cell_clear_resets_has_extras) | 2 | Tests the #[cfg(test)] Cell::clear (assigns EMPTY); no production caller. | **delete** |
| `src/cell_tests.rs` (cell_char_data_at_byte_offset_zero, cell_colors_at_byte_offset_two, cell_flags_at_byte_offset_six) | 3 | Pins field offsets of a #[repr(C, packed)] struct by transmuting to u64; repr and field order already fix them. Replace with const offset_of! asserts beside the existing size assert. | **delete** |
| `src/damage/property_tests.rs` (merged_damage_iterator_full_damage_yields_single_rect); `src/damage/tests.rs` (line_damage_bounds_merge, line_damage_bounds_no_merge_gap, damage_rect_extend, damage_iter_merged_consecutive_overlapping, damage_iter_merged_non_overlapping, damage_iter_merged_gap, damage_iter_merged_full_rows, damage_rect_dimensions, damage_rect_dimensions_consistent, line_damage_bounds_is_empty, damage_rect_from_line) | 12 | Tests MergedDamageIterator, DamageRect and LineDamageBounds::{new,is_empty,can_merge_with,merge_with}, all cfg(test) or cfg(any(test, feature="testing")) with no production caller (aterm-gui has its own present::DamageRect). | **delete** |
| `src/extra_collection_shifts.rs` (test_shift_region_out_of_region_extras_skip_drain_rebuild) | 1 | Pins an optimization (no drain-rebuild when no entry lies in the region) through HashMap value pointer identity; extras-shifts subsumes its functional content. | **move-to-bench** |
| `src/extra_tests.rs` (cell_extra_size_optimized) | 1 | Pins size_of::<CellExtra>() == 64, a memory budget, not behaviour. Replace with a const assert, as Cell already has. | **delete** |
| `src/extra_tests.rs` (is_combining_mark_basic, is_combining_mark_all_ranges) | 2 | is_combining_mark is cfg(any(test, feature="testing")) with no production caller (aterm-core re-exports it only under cfg(test)). | **delete** |
| `src/extra_tests.rs` (row_has_hyperlinks_detection, row_has_hyperlinks_partial_removal) | 2 | Tests the #[cfg(test)] observer CellExtras::row_has_hyperlinks (a linear scan, not a cache); extras-map-basic and cell-extra-fields cover what it samples. | **delete** |
| `src/grid/buffer_access.rs` (test_resolved_char_supplementary_plane_emoji, test_resolved_char_bmp_unchanged, test_row_text_includes_supplementary_plane) | 3 | Duplicates content_queries.rs resolved_char/row_text tests (same function, same input); grid-cell-text-reads subsumes them. | **delete** |
| `src/grid/cursor_ops.rs` (move_cursor_by_positive_offset, move_cursor_by_negative_offset, move_cursor_by_clamps_to_zero, move_cursor_by_clamps_to_max) | 4 | Grid::move_cursor_by is cfg(any(test, fuzz)) with no production caller; its behaviour is cursor-set-clamp after clamp_u16. | **delete** |
| `src/grid/cursor_ops.rs` (backspace_at_col_zero_stays, backspace_clears_pending_wrap) | 2 | Grid::backspace is #[cfg(test)]; production BS is cursor_backward(1) (cursor-horizontal-margins). | **delete** |
| `src/grid/cursor_ops.rs` (save_and_restore_cursor_roundtrip, restore_cursor_without_save_is_noop, restore_cursor_clamps_to_resized_grid); `src/grid/state/storage.rs` (cursor_save_and_restore); `src/grid/tests/algorithm_boundary/pending_wrap.rs` (save_restore_cursor_preserves_pending_wrap) | 5 | Grid::save_cursor/restore_cursor and GridCursorState::save_cursor are #[cfg(test)] copies of DECSC/DECRC; the production DECRC lives in the terminal layer, whose spec should carry this. | **delete** |
| `src/grid/cursor_ops.rs` (accessors_consistent_with_cursor_struct, accessors_after_sequential_moves) | 2 | Tautology (cursor_row()/cursor_col() are defined as cursor().row/.col) or a duplicate of the motion specs through the cfg(test) backspace. | **delete** |
| `src/grid/pin_tests.rs` (pin_creation, pin_from_absolute, pin_with_modifications, pin_validity, evict_pages_from, pinned_range, pinned_range_normalized, pinned_range_validity, generation_tracker_new, generation_tracker_evict); `src/pin.rs` (test_pin_new_stores_all_fields, test_pin_from_absolute_zero, test_pin_from_absolute_small_row, test_pin_from_absolute_large_row_roundtrip, test_pin_from_absolute_max_u32_row, test_pin_with_col_updates_column_only, test_pin_with_row_offset_updates_row_only, test_generation_tracker_new_starts_at_zero, test_generation_tracker_page_generation_unknown_page_is_zero, test_generation_tracker_ensure_capacity_grows, test_generation_tracker_ensure_capacity_idempotent, test_generation_tracker_ensure_capacity_grows_larger, test_evict_page_increments_page_generation, test_evict_page_increments_global_generation, test_evict_page_does_not_affect_other_pages, test_evict_page_multiple_times, test_evict_page_auto_grows_capacity, test_evict_pages_from_evicts_all_from_start, test_evict_pages_from_increments_global_once, test_evict_pages_from_beyond_capacity_is_noop, test_evict_all_increments_all_pages, test_evict_all_updates_min_valid_generation, test_is_valid_fresh_pin, test_is_valid_after_eviction_returns_false, test_is_valid_after_evicting_different_page, test_is_valid_pin_with_future_generation, test_is_potentially_valid_fresh_pin, test_is_potentially_valid_after_eviction_returns_false, test_is_potentially_valid_allows_future_generation, test_is_potentially_valid_below_min_valid_returns_false, test_is_valid_recreated_pin_after_eviction, test_evict_all_invalidates_all_pages, test_evict_all_then_recreate_pins_are_valid, test_pinned_range_is_valid_both_pins_valid, test_pinned_range_is_valid_start_invalid, test_pinned_range_is_valid_end_invalid, test_pinned_range_is_valid_both_invalid, test_pinned_range_normalized_already_ordered, test_pinned_range_normalized_swaps_reversed, test_pinned_range_normalized_same_row_different_col, test_pinned_range_normalized_same_row_same_col, test_pinned_range_normalized_same_row_ordered_col, test_pinned_range_normalized_preserves_generation, test_pinned_range_normalized_is_idempotent, test_sequential_evictions_accumulate, test_mixed_evict_page_and_evict_all) | 56 | Pin, PinnedRange and every GenerationTracker reader are #[cfg(test)]; production writes generations (grid/scroll.rs evict_page) but nothing reads them. pin_tests.rs also duplicates pin.rs's own tests. | **delete** |
| `src/grid/scroll_convert.rs` (plain_run_equivalence_rejects_lost_text_attributes_and_wrap) | 1 | Tests the test module's own comparator (old_cell_loop/equivalent), not production code; obsolete once deferred-line-equals-eager is certified. | **delete** |
| `src/grid/scrollback_offload.rs` (reflow_step_timing) | 1 | An #[ignore]d wall-clock timing harness that prints microseconds per line; samples no property. | **move-to-bench** |
| `src/grid/state/storage.rs` (a_row_replaced_by_a_fresh_allocation_is_seen_by_both_readings) | 1 | Negative control for the test-only ring_slice_identities helper: tests the oracle, not production. The mutation control belongs to the proof of ring-row-shift-permutation. | **delete** |
| `src/grid/tests/reflow/cost_bound.rs` (offloaded_resize_keeps_synchronous_reflow_within_the_ring) | 1 | Duplicate: its bound (sync ≤ rows·8) and history checks are implied by offloaded_resize_synchronous_rewrap_is_zero (sync = 0), as its own doc says. | **delete** |
| `src/grid/tests/style_perf_complexity.rs` (reflow_linear_time, reflow_linear_time_columns) | 2 | Ratio test on deterministic counters (a proxy for linearity); the exact counter law is reflow-cost. Delete once reflow-cost is certified. | **becomes-spec-after-fix** |
| `src/grid/tests/style_perf.rs` (grid_write_char_with_style_id_default, grid_write_char_with_style_id_indexed_color, grid_write_char_with_style_id_rgb_color, grid_write_char_with_style_id_extra_flags, grid_style_id_deduplication_in_writes, grid_write_char_wrap_with_style_id, grid_write_wide_char_with_style_id, grid_write_wide_char_wrap_with_style_id) | 8 | Tests #[cfg(test)] StyleId grid writers (write_test_helpers.rs, write_char_with_style_id, write_wide_char_with_style_id, write_*_wrap_with_style_id) and the #[cfg(test)] interner (intern_extended); production never interns, so style-table-new-holds-only-default covers what ships. | **delete** |
| `src/grid/tests/stale_extras_overwrite.rs` (write_ascii_run_styled_clears_stale_extras_entries, failed_wide_write_does_not_drop_live_extras, emoji_run_autowrap_tightens_len_when_the_run_orphans_the_row_tail) | 3 | Tests #[cfg(test)] write paths (write_ascii_run_styled, write_wide_char_at_cursor_packed, write_emoji_run_autowrap); the production paths are writes-replace-extras. | **delete** |
| `src/grid/tests/scrollback.rs` (grid_attach_detach_scrollback) | 1 | Grid::detach_scrollback is #[cfg(test)]; the rest is a getter tautology. | **delete** |
| `src/grid/tests/tab_stops.rs` (grid_is_tab_stop, grid_tab_stop_positions) | 2 | GridCursorState::is_tab_stop / tab_stop_positions are #[cfg(test)]; contents covered by tab-default-stops and tab-stop-set-clear via tab_stops(). | **delete** |
| `src/page_tests/pointer_correctness.rs` (alloc_slice_write_read_roundtrip_after_reset, reset_three_pages_all_zeroed_on_reuse); `src/page_tests/store_ops.rs` (reset_moves_pages_to_free_list, reused_pages_are_zeroed, lazy_zeroing_only_zeros_used_portion, lazy_zeroing_handles_multiple_pages, page_used_bytes_tracked_correctly, resetting_pages_discards_recycled_handles_before_page_reuse); `src/page_tests/thread_safety.rs` (allocate_reset_cycle_always_zeroed, allocate_reset_cross_page_zeroed, lazy_zeroing_small_then_large_reallocation) | 11 | Exercise #[cfg(test)] PageStore::reset and the lazy-zero path only it feeds (reset is also an unsound safe API, §5). | **delete** |
| `src/page_tests/pointer_correctness.rs` (offset_out_of_bounds_returns_none, offset_near_boundary_u32); `src/page_tests/store_ops.rs` (offset_roundtrip); `src/page_tests/thread_safety.rs` (offset_get_mut_non_overlapping_offsets, offset_get_mut_then_get_same_offset, offset_get_bounds_and_alignment, offset_get_mut_roundtrip) | 7 | Offset<T>::get/get_mut are #[cfg(test)] scaffolding with no production caller. | **delete** |
| `src/page_tests/store_ops.rs` (stats_tracking_accurate) | 1 | Asserts only PoolStats counters, readable only via the #[cfg(test)] stats(); observable behaviour is page-pool-reuse-and-memory. | **delete** |
| `src/page_tests/thread_safety.rs` (guard_page_send_sync, guard_page_slice_send_sync) | 2 | Compile-time Send/Sync bound checks the type system already guarantees (page.rs::_guard_page duplicates them). | **delete** |
| `src/row/tests/algorithm_audit.rs` (clear_from_zero_sets_len_to_zero); `src/row/tests/basic_ops.rs` (row_clear_from, row_clear_from_sparse_tail) | 3 | Row::clear_from is #[cfg(test)] with no production caller (production clears through clear_range_with). | **delete** |
| `src/row/tests/style_id.rs` (row_write_char_with_style_id_basic, row_write_char_with_style_id_preserves_flags, row_write_char_with_style_id_out_of_bounds, row_write_char_with_style_id_overwrites_wide_continuation, row_write_wide_char_with_style_id_basic, row_write_wide_char_with_style_id_at_edge, row_write_wide_char_with_style_id_overwrites_existing_wide, row_write_wide_char_with_style_id_preserves_flags, write_char_with_style_id_shrinks_len_on_tail_wide_head_overwrite, write_wide_char_with_style_id_shrinks_len_on_tail_wide_orphan); `src/row/tests/wide_char_fixup.rs` (style_id_wide_write_sets_has_wide_chars_for_later_narrow_overwrite); `src/row/tests/wide_spacer_rendition.rs` (write_wide_char_with_style_id_inverse_reaches_the_spacer) | 12 | Tests the #[cfg(test)] StyleId row writers (row/style_id_write.rs), which also skip note_write_through; the laws they check are row-narrow-write, row-wide-write and wide-continuation-of. | **delete** |
| `src/row/tests/unsafe_boundary/unchecked.rs` (get_unchecked_out_of_bounds_panics_in_debug, get_unchecked_mut_out_of_bounds_panics_in_debug) | 2 | A debug_assert on a violated unsafe precondition; once row-get-unchecked's requires is a proved obligation at every caller it samples nothing. | **delete** |
| `src/row/tests/wide_char_fixup.rs` (selective_erase_chars_clears_left_boundary_continuation, selective_erase_chars_shrinks_len_on_tail_wide_orphan) | 2 | Row::selective_erase_chars is a #[cfg(test)] implementation (not a delegation) with no production caller. | **delete** |
| `src/style_color.rs` (grid_ansi_table_matches_color_palette) | 1 | Color::from_ansi_256, compute_ansi_256 and ANSI_256_TABLE are #[cfg(test)] (their only caller, Style::unpack_color, is too). | **delete** |
| `src/style_table.rs` (test_intern_extended_dedup_no_extra_entry, test_get_interned_style, test_intern_extended_twice_refcount_is_2, test_release_at_zero_does_not_underflow, test_release_out_of_bounds_is_safe, test_intern_many_distinct_styles, test_intern_style_with_attrs, test_intern_style_indexed_fg, test_intern_style_rgb_bg, test_intern_style_both_indexed, test_extended_preserves_color_type_info, test_try_intern_l1_miss_on_empty_cache, test_try_intern_l1_hit_after_intern_extended, test_try_intern_l1_miss_for_different_style, test_try_intern_l1_increments_refcount, test_insert_new_style_returns_sequential_ids, test_clear_leaves_only_default, test_clear_resets_l1_cache, test_clear_preserves_lookup_for_default, test_intern_after_clear, test_build_compaction_map_does_not_mutate, test_build_compaction_map_dead_maps_to_default, test_build_compaction_map_live_count, test_indexed_l2_cache_hit, test_rgb_l2_cache_hit, test_intern_default_style_returns_default_id, test_intern_default_style_increments_default_refcount); `src/style_tests.rs` (intern_same_style, intern_different_styles, table_release, table_release_default_not_decremented, table_get_id, table_stats, table_clear, intern_extended_style, style_table_cap_at_u16_max, style_table_saturation_with_dead_styles_degrades_to_default, style_table_saturation_preserves_existing_styles, build_compaction_map_is_dense_and_read_only, build_compaction_map_all_live_is_identity) | 40 | StyleTable interning, refcounts, release, clear, compaction and the L1/L2 caches are #[cfg(test)]; in production the table is always StyleTable::new(). | **delete** |
| `src/style_tests.rs` (style_id_default, color_is_default, style_constructors, style_is_default, style_setters, extended_style_from_cell_style_default, extended_style_from_cell_style_indexed, extended_style_from_cell_style_rgb) | 8 | Tests #[cfg(test)] Style/Color/StyleId/ExtendedStyle helpers (style_test_helpers.rs) or struct-update syntax the type system guarantees. | **delete** |
| `src/style_tests.rs` (style_intern_constant_time) | 1 | 'Complexity' test whose counter is incremented once per intern call by the test path itself; holds by construction for any lookup cost, and tests #[cfg(test)] code. | **delete** |
| `src/terminal_style.rs` (test_single_underline, test_double_underline, test_dotted_underline, test_dashed_underline) | 4 | Bitflag insert/contains on constants, or a const-definition tautology; cell-flags-layout pins the bits and current-style-new-fields covers storage. | **delete** |
| `src/terminal_style.rs` (test_build_style_default, test_build_style_with_indexed_fg, test_build_style_with_bold, test_build_extended_style_rgb_fg, test_build_extended_style_indexed_bg, test_build_extended_style_fg_changed, test_build_extended_style_bg_changed, test_build_extended_style_both_changed) | 8 | CurrentStyle::build_style / build_extended_style* and Color::from_ansi_256 are #[cfg(test)]; the cache half of the *_changed variants is current-style-cache-invariant. | **delete** |
| `src/terminal_style.rs` (test_saved_cursor_state_default, test_saved_cursor_state_preserves_style) | 2 | Derived Default of SavedCursorState, or a struct literal read back; no function under test. | **delete** |
| `tests/grid_miri.rs` (miri_row_cells_mut_bulk_write) | 1 | Row::cells_mut exists only under cfg(miri); no production caller. | **delete** |
| `tests/security_regression_5573.rs` (row_new_and_resize_stay_unsafe_fns) | 1 | The check is the compile-time trait-ambiguity and const fn-pointer pins, which rustc enforces whether or not the #[test] exists; keep the const pins, delete the #[test] wrapper. | **delete** |

Totals by action: delete 240, move-to-bench 2, becomes-spec-after-fix 2.

## 5. Implied specs that look wrong, or disagree with the code

1. **The six passes disagreed about `#[cfg(test)]` code; this merge resolves it one way.** A test of a
   `#[cfg(test)]` function with no production twin is in §4, wherever it sits.
   - `GenerationTracker::evict_page` and `ensure_capacity` are production, but every reader of their
     state is `#[cfg(test)]`. One pass gave `pin_tests.rs::generation_tracker_{new,evict}` a spec. Here they
     are §4 deletes, together with `pin.rs`'s 46 tests. Recommendation: delete `GenerationTracker` together
     with `Pin`/`PinnedRange`, or give it a production reader and then a spec.
   - `Grid::save_cursor`/`restore_cursor` and `GridCursorState::save_cursor` are `#[cfg(test)]`. One pass
     put a save/restore clause into the pending-wrap table. That clause is dropped here, and
     `storage.rs::cursor_save_and_restore` and `pending_wrap.rs::save_restore_cursor_preserves_pending_wrap`
     are §4 deletes. The DECSC/DECRC spec belongs to the terminal layer, which owns the shipped code.
   - `StyleTable::intern_extended` and `Grid::intern_extended_style` are `#[cfg(test)]`. One pass stated
     `style-intern-resolve` "on the production pair", but production never interns. That spec is dropped,
     and its 5 tests in `style_perf.rs` are §4 deletes. `style-table-new-holds-only-default` proves the
     `USES_STYLE_ID` branch dead instead.
   - `CellExtras::row_has_hyperlinks` is a `#[cfg(test)]` linear scan. There is no per-row cache, so the
     "cache coherence" clause one pass gave `hyperlink_perf.rs` was vacuous. Those tests stay because they
     sample map operations, and they map to `extras-shifts`/`extras-clear-ops`. Retarget their observer to
     `Ext` (any `get(row,c).hyperlink()`).
   - These tests drive `#[cfg(test)]` wrappers of production functions and should be **retargeted**, not
     deleted, when their spec is proved:
     - `write_char_wrap` (→ `write_char_wrap_styled`)
     - `write_ascii_run_styled` (→ `write_ascii_blast`/`write_cell_run`)
     - `scroll_down` (→ `scroll_region_down` on the full region)
     - `checkpoint_lines` (→ `checkpoint_lines_bounded(usize::MAX)`)
     - `tab_n`/`back_tab_n` (→ the `_margin` forms)
     - `is_tab_stop` (→ `tab_stops()`)
     - `write_ops.rs::backspace_from_mid_column` (→ `cursor_backward(1)`)
   - `Row::copy_from` has two textually identical bodies (`#[cfg(miri)] pub` and `#[cfg(not(miri))] pub(crate)`),
     so `miri_row_copy_from` exercises the copy. `Row::cells_mut` exists only under `cfg(miri)`.
2. **Code that contradicts its docs or invites a bug**
   - `src/damage/iter.rs` `DamagedRowIterator::size_hint` (Full arm) computes `*max - *current` unchecked. The
     fields are `pub` and `#[non_exhaustive]` sits on the enum, not the variant, so outside code can build
     `Full{current > max}` and panic in debug. `full_iter_size_hint_exact` implies a `requires current ≤ max`
     that nothing enforces.
   - `damage/tests.rs::tracker_mark_cell_u16_max_saturates_right_bound` pins bounds `(u16::MAX, u16::MAX)` after
     `mark_cell(row, u16::MAX)`. The row is flagged damaged with an empty interval, so `row_damage_bounds` is None
     and `iter_bounds` never redraws the cell. `MAX_GRID_COLS = 4096` makes this unreachable, but the spec must
     state the saturating hull or require `col < u16::MAX`.
   - `RingSpans::ring_row` normalizes only `external_row + ring_offset < 2·V`. An external row in
     `[V, 2V - offset)` silently aliases a live ring row. The "stale row ≥ visible_rows → no writes" case comments in
     `clear_range_matches_per_cell_clear` and `complex_clear_range_matches_per_cell_clear` hold only because
     9 ≥ 2·4. The ring specs carry `requires row < visible_rows`.
   - `CellExtras::internal_row` uses `row.wrapping_add(row_offset)`. A row with `row + row_offset > u16::MAX`
     wraps onto stale entries below `row_offset`. This is unreachable while rows < 4096 and offset < 256, and
     the overflow tests use rows near `u16::MAX` only with offset 0.
   - `CellExtras::is_empty()` counts stale entries below `row_offset`, while `len()` excludes them. After
     `scaling_proofs.rs::scroll_compound_cost_amortized_o1`, `len() == 0` but `is_empty() == false`. Callers only lose
     a fast path, but the two observers disagree.
   - `CellExtras::shift_rows_up_by(start_row > 0, n)` clears both dense rings wholesale, which contradicts its
     doc ("Rows < start_row are preserved unchanged"). No production caller passes `start_row > 0`
     (`grid/scroll.rs:375,434` and `grid/reflow.rs:1440` pass 0), and the tests that do check only the map. Drop
     the parameter or fix the ring branch before proving `extras-shifts` for s > 0.
   - `CellExtras::clear_row`, `clear_range` and `clear_rows` blank the RGB ring but not the complex-char ring;
     only `clear_rect` blanks both. `complex_codepoint_for` and `complex_char_str_for` read the ring without
     checking `COMPLEX`, so a stale non-BMP char survives EL/ED in the ring. `render_data_for_cell` is safe
     because it gates on `is_complex`. Confirm this is intended.
   - `Cell::is_empty` reads only the colour mode nibbles, so a blank with HAS_EXTRAS or stale index bits still
     reports empty. Check whether any caller expects "no extras" from it.
   - `CellExtra::set_extended_flags` shifts `ext << 3` inside a `u16` and drops bits 13-15. Its only reader,
     `extended_flags()`, is `#[cfg(test)]`. The `requires ext < 1<<13` is untested.
   - `CellExtra::set_underline_color_u32` treats every type byte other than 0x02 as RGB (0x00 and 0xFF
     included), while `PackedColor` uses 0x00 = indexed and 0xFF = default. Check the producers in
     `aterm-core` `handler_write.rs:676,961`.
   - `<Row as Display>::fmt` skips every cell with raw bit 10, so a DECSCA-protected glyph disappears from
     `to_string`/`Debug`. Every other reader uses the context-aware `is_cell_wide_continuation`.
   - `PageStore::reset` is a safe `pub fn` whose doc has a `# Safety` section, because outstanding
     `PageSlice`s alias the recycled pages. That is an unsound safe API, though test-only. It also makes
     `alloc_page`'s lazy-zero branch unreachable in production.
   - `SelectionDamage::union`'s doc says bands "degrade to [their] hull past MAX". `from_merged` merges the
     closest adjacent pair instead and never takes the hull.
   - The `cursor_up`/`cursor_down` ENSURES docs (`old(cursor.row in scroll_region) implies …`) are weaker
     than the xterm rule the code implements. Upgrade them to `cursor-vertical-margins` when they become
     Trust clauses.
   - `GridStorage::effective_cols_for_row` relies on the invariant "`any_double_width` holds whenever some
     row is double width or double height". The `storage.rs` `effective_cols_*` tests set the flag by hand
     and bypass the code that maintains it. Without the invariant, `line_size` is ignored.
   - `Row::write_char` runs the wide fixup (`get_mut` on col-1 / col+1) before its bounds check. This is
     harmless with `get`/`get_mut`, but it would be out of bounds after a refactor to `get_unchecked`.
     `row-narrow-write` states the false-return path explicitly.
   - Docs for code that is test-only: `StyleTable::build_compaction_map` claims checkpoint serialization
     uses it, and every `StyleTable` mutation (interning, refcounts, compaction) is `#[cfg(test)]`. The
     refcount docs are stale for release builds.
   - Inconsistent conversions: `from_cell_style` maps an indexed colour to `DEFAULT_*`, while
     `from_packed_colors_separate` maps it to `from_ansi_256(idx)` (both test-only).
   - Bodies that do not match their names: `build_extended_style_{fg,bg,both}_changed` have identical
     bodies.
   - `unscroll_from_scrollback` returns 0 while its fallback moves content. So the return value counts the
     history lines recovered, not the rows moved; `unscroll-denotation` says so.
3. **Mislabelled, stale or vacuous tests**
   - `grid/content_queries.rs::test_row_text_null_cells_become_spaces`: unwritten cells are `Cell::EMPTY`
     (U+0020), not nulls.
   - `grid/checkpoint_lines.rs::visible_checkpoint_cost_does_not_scale_with_scrollback` asserts no cost.
   - `extra_collection_shifts.rs::{test_shift_region_down_single_row_shift, test_decstbm_scroll_up_within_margins,
     test_decstbm_scroll_down_within_margins, test_declrmm_rect_scroll_down}` use weak disjunctions ("gone or
     overwritten") where the code guarantees "absent".
   - `erase.rs::erase_line_within_margins_gives_the_wide_orphan_the_bce_background_and_ring` describes a
     margin-bounded EL 2, but EL 2 erases the whole row. The orphan rule it names is never sampled.
   - `erase.rs::test_decaln_resets_scroll_region` never sets a non-full region first, so a missing reset
     could not fail it.
   - `erase.rs::test_el1_el2_and_el0_on_the_continuation_keep_the_link` ends with an EL0 and no
     assertion (an implicit no-panic check).
   - `erase.rs::test_erase_scrollback_no_scrollback` samples only the zero-history branch. The rebuild
     branch is sampled in `scrollback.rs`.
   - `cursor_ops.rs::line_feed_at_bottom_of_region_scrolls` and `reverse_line_feed_at_top_of_region_scrolls_down`
     check only the cursor row, not the scroll they are named for.
   - `line_ops.rs::test_ich_clears_pending_wrap` has contradictory comments and arms the wrap through
     `storage.mark_pending_wrap`.
   - `line_ops.rs::test_{ich,dch,ech}_*wide_char*` never check that the pair stays well formed.
   - `scroll.rs::a_drained_flood_backlog_gives_its_spine_back` asserts `capacity ≤ 4000`, which relies on
     `VecDeque::shrink_to` behaviour that std leaves unspecified. `lazy-spine-slack-bound` states only the call.
   - `scroll.rs::a_marker_shrinks_as_the_window_slides_past_its_hole` and
     `a_window_that_slides_past_the_whole_hole_takes_the_marker` rig the cut by writing fields. In
     production that state is reachable only through `bound_detached_lazy_buffer`, so the spec's
     precondition should name that path.
   - `reflow.rs::last_resize_shape_in_the_trim_demote_and_bottom_push_regimes` samples three specs at once,
     and its retention-0 halves duplicate `resize_undo.rs`.
   - `tests/deferred_line_equivalence.rs` (the `assert_lines_equivalent` helper used by 17 tests) passes a
     **byte** offset from `char_indices()` to `Line::get_attr`, which takes a **char** index. On non-ASCII
     rows it compares attrs at the wrong positions. `deferred-line-equals-eager` compares by char index.
   - `tests/boundary.rs::write_ascii_blast_and_char_wrap_cursor_consistency` says both writers wrap
     immediately; both defer.
   - `tests/algorithm_boundary/history_extras.rs::shift_region_{up,down}_single_row_region` have the
     "below"/"above region" labels swapped.
   - `tests/algorithm_boundary/scrolling.rs::scroll_region_*_top_equals_bottom_is_noop` poke a region that
     `set_scroll_region` cannot produce. `scroll-region-shift` covers it explicitly as a defensive state.
   - `tests/algorithm_boundary/resize_and_invariants.rs::reflow_shrink_cursor_past_content_end` and
     `reflow_grow_cursor_in_trailing_blank_of_continuation_row` assert only what CursorInBounds already
     implies.
   - `tests/boundary.rs::grid_insert_chars_at_col_zero` claims a shift it never checks.
   - `tests/algorithm_boundary/scrolling.rs::{line_feed,reverse_line_feed,write_char_wrap}_after_scroll_display_resets_offset`
     say "does not panic" but pin display_offset = 0.
   - `tests/extras_performance.rs` ratio tests check deterministic counters against loose windows
     (0.5..1.5, 1.5..3.0, < 3.0). `extras-op-cost` and `reflow-cost` state the exact counts, and three
     tests check the same `scroll_up` claim.
   - `scrollback_offload.rs::stepping_interleaved_with_window_output_keeps_history_and_order` checks
     conservation with slack (`+ 8`). `offload-refines-plain-resize` states equality.
   - `scroll_fill.rs`: 10 tests exercise scroll ops, not fill, and duplicate `algorithm_boundary/scrolling.rs`.
   - `tests/style_perf.rs::row_access_at_ring_tiered_boundary` asserts `r.len() == cols()` on an accessible
     row. `Row::len` is the written high-water mark, so the branch passes only because it is vacuous.
   - `scroll_region/unscroll.rs::grid_unscroll_wide_char_at_last_col_dropped` never creates a wide char at
     the last column, because the write is rejected.
   - `scroll_region/unscroll.rs::grid_unscroll_preserves_wrapped_flag` hand-sets a continuation flag on
     row 0.
   - `scroll_region/unscroll.rs::grid_unscroll_with_scroll_region` uses a `||` assertion that does not
     fix which lines land where.
   - `reflow/cost_bound.rs::naive_width_resize_rewraps_whole_history_synchronously` pins an undesirable
     cost as a lower bound. `width-resize-sync-cost` states the exact equation.
   - `reflow/cost_bound.rs::offload_window_lazy_buffer_is_bounded`'s 45k..52k window hides the exact law
     (newest CAP kept, every drop counted).
   - `style_perf.rs::clear_hyperlink_clears_has_extras_when_empty` performs the flag sync itself, so its
     last assertion checks the test's own code.
   - `reflow/core.rs::reflow_grow_unwraps_soft_wrapped_lines` uses a disjunctive final assertion.
   - Files and headers named for something they do not hold:
     - `style_perf.rs` holds no performance tests.
     - `style_perf_complexity.rs::row_to_line_{extracts,no}_hyperlinks` are not complexity tests and go
       through a `#[cfg(test)]` helper.
     - `reflow/performance.rs` calls its sampled counter checks "proofs".
     - `scrollback_materialize.rs` has a stale `advance_grapheme_unit tests` header.
   - `state/presentation.rs::union_is_order_independent_past_the_arity_bound` never exceeds the 8-band
     bound it is named for, and its comment says the bound is 4. Order independence past the bound does
     not hold: `from_merged` absorbs pairs order-dependently.
   - `page_tests/pointer_correctness.rs::alloc_zero_length_slice_is_stable_and_non_consuming` claims a
     len 0 alloc never advances the offset, but it does align it. The claim holds only for two `u32`
     allocations.
   - `style_tests.rs::style_intern_constant_time` counts calls, not work.
   - `tests/history_renumber_epoch.rs::a_width_reflow_with_no_scrollback_still_advances_the_renumber_epoch`
     calls a same-size resize but describes it as "rows-only".
4. **Negative controls and model checks are not specs.**
   - These tests are the mutation controls that step 3 of "done" requires. Each one moves into its
     spec's proof as the negative control and need not survive as a `#[test]`:
     - `conformance_retention.rs::resurrected_pre_unification_split_is_rejected`
     - the forged-observation halves of the conformance tests
     - `storage.rs::a_row_replaced_by_a_fresh_allocation_is_seen_by_both_readings`
   - The Tier-0 `ty check` tests (`*_spec_model_checked_in_trust`) become verify-model obligations.
   - `tests/conformance_offload.rs` models the detach window synchronously, but production may step the
     job on a worker. That is why `offload-window-refinement` lists gap 13.

## 6. The three specs I would prove first (easiest, high value)

1. **The `CellFlags` trio**: `cell-flags-bit-algebra`, `wide-continuation-of` and `cell-flags-layout`.
   - Each is a `u16` `#[repr(transparent)]` newtype with `&`, `|`, `!` and consts, with no loops, heap or
     panics.
   - The quantified clauses are over `u16`, so they are decidable by evaluation. The only real widening
     is gap 3 (bitwise ops in E6 bodies).
   - It retires 13 tests. Every row, write, erase and reflow spec cites `bit10`/`WIDE`/`COMPLEX` through
     these facts, so it is the base of the whole cell layer.
2. **The reflow chunk kernels**: `cursor-in-chunk` and `adjust-chunk-boundary`.
   - Both are pure scalar functions over `usize`/`u16` and one `&[Cell]` read. `adjust-chunk-boundary`
     carries a progress clause (result > o) that is the decreases measure `chunk-cells-to-rows` needs.
   - They retire 15 tests.
   - They are the lemmas `reflow-rewrap-layout` (29 primary tests), `reflow-cursor-tracking` (10) and
     `width-roundtrip-restores-topology` (8) are built on.
3. **`display-offset-damage`**.
   - It is usize/u16 subtraction and comparison returning an enum, plus a no-panic obligation: both
     `u16::try_from(..).expect()` calls are unreachable.
   - It retires 6 tests, including the exhaustive delta × size sweep.
   - It is the arithmetic core of `viewport-scroll-damage` (11 tests). It is also the cleanest first case
     for gap 4 (callee-ensures composition) through `apply_display_offset_damage`.

Runner-up: the cursor-motion family. `cursor-set-clamp`, `cursor-vertical-margins`,
`cursor-horizontal-margins`, `carriage-return` and `cursor-in-bounds` retire 47 tests with scalar u16
arithmetic, and the two pending-wrap tables (`pending-wrap-motion-table`, `erase-pending-wrap-table`) retire 30 more. They wait only on
gap 2 (certified `&mut self` views), so they are the biggest cheap collapse once that gap lands.
