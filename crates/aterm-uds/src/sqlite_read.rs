// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure-Rust, read-only SQLite 3 B-tree and WAL snapshot reader (`#![forbid(unsafe_code)]`).
//!
//! Reads committed table rows directly from in-memory `.db` and optional `-wal` byte slices
//! captured lock-free by [`crate::foreign_db::read_bytes`], spawning no subprocess and taking
//! no POSIX or shared-memory lock.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// Maximum assembled payload bytes for a single B-tree cell across overflow pages.
const MAX_CELL_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Maximum rows visited across a single table scan.
const MAX_VISITED_ROWS: usize = 1_048_576;

/// Maximum matching rows collected in a single table scan.
const MAX_COLLECTED_ROWS: usize = 65_536;

const SQLITE_HEADER_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const WAL_MAGIC_LE: u32 = 0x377f_0682;
const WAL_MAGIC_BE: u32 = 0x377f_0683;
const WAL_HEADER_LEN: usize = 32;
const WAL_FRAME_HEADER_LEN: usize = 24;

/// Decoded column value from a SQLite record.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue<'a> {
    /// SQL `NULL`.
    Null,
    /// Signed 64-bit integer (`INTEGER`).
    Int(i64),
    /// IEEE-754 floating-point number (`REAL`).
    Float(f64),
    /// Decoded UTF-8 text (`TEXT`).
    Text(Cow<'a, str>),
    /// Raw byte slice (`BLOB`).
    Blob(Cow<'a, [u8]>),
}

impl SqlValue<'_> {
    /// Borrows a text value when this cell is `TEXT`.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_ref()),
            Self::Null | Self::Int(_) | Self::Float(_) | Self::Blob(_) => None,
        }
    }

    /// Returns the integer value when this cell is `INTEGER`.
    #[must_use]
    pub const fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            Self::Null | Self::Float(_) | Self::Text(_) | Self::Blob(_) => None,
        }
    }

    /// Formats a scalar cell the same way `sqlite3 -list` prints it.
    #[must_use]
    pub fn to_list_field(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Int(n) => n.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Text(s) => s.as_ref().to_owned(),
            Self::Blob(b) => String::from_utf8_lossy(b).into_owned(),
        }
    }

    /// Compares the `sqlite3 -list` representation of this cell against `expected` without
    /// allocating on `NULL`, `TEXT`, or valid UTF-8 `BLOB` cells.
    #[must_use]
    pub fn list_field_eq(&self, expected: &str) -> bool {
        match self {
            Self::Null => expected.is_empty(),
            Self::Text(s) => s.as_ref() == expected,
            Self::Blob(b) => String::from_utf8_lossy(b).as_ref() == expected,
            Self::Int(_) | Self::Float(_) => self.to_list_field() == expected,
        }
    }

    /// Converts any borrowed backing slice into an owned `'static` value.
    #[must_use]
    pub fn into_owned(self) -> SqlValue<'static> {
        match self {
            Self::Null => SqlValue::Null,
            Self::Int(n) => SqlValue::Int(n),
            Self::Float(f) => SqlValue::Float(f),
            Self::Text(s) => SqlValue::Text(Cow::Owned(s.into_owned())),
            Self::Blob(b) => SqlValue::Blob(Cow::Owned(b.into_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Parsed column metadata from a `CREATE TABLE` DDL statement in `sqlite_schema`.
#[derive(Clone, Debug, PartialEq)]
pub struct TableSchema {
    columns: Vec<String>,
    defaults: Vec<SqlValue<'static>>,
    rowid_alias_col: Option<usize>,
    disk_to_table: Option<Vec<usize>>,
}

impl TableSchema {
    /// Looks up a column index by ASCII case-insensitive column name.
    #[must_use]
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|col| col.eq_ignore_ascii_case(name))
    }
}

/// Consistent in-memory page view over a SQLite main file and its committed WAL frames.
struct Snapshot<'a> {
    db: &'a [u8],
    committed_wal: HashMap<u32, &'a [u8]>,
    page_size: usize,
    usable_size: usize,
    effective_pages: u32,
    encoding: TextEncoding,
}

impl<'a> Snapshot<'a> {
    fn open(db: &'a [u8], wal: Option<&'a [u8]>) -> Option<Self> {
        let main_ps = if db.len() >= 100 {
            if &db[..16] != SQLITE_HEADER_MAGIC {
                return None;
            }
            Some(decode_page_size(u32::from(read_u16_be(db, 16)?))?)
        } else if db.is_empty() {
            None
        } else {
            return None;
        };

        let (committed_wal, wal_pages, wal_ps) = match wal {
            Some(wal_bytes) if !wal_bytes.is_empty() => parse_wal(wal_bytes, main_ps)?,
            _ => (HashMap::new(), None, None),
        };

        let page_size = main_ps.or(wal_ps)?;
        let provisional_pages = wal_pages.unwrap_or_else(|| {
            u32::try_from(db.len() / page_size)
                .unwrap_or(u32::MAX)
                .max(u32::from(committed_wal.contains_key(&1)))
        });
        if provisional_pages == 0 {
            return None;
        }

        let mut snap = Self {
            db,
            committed_wal,
            page_size,
            usable_size: page_size,
            effective_pages: provisional_pages,
            encoding: TextEncoding::Utf8,
        };

        let page1 = snap.page(1)?;
        if page1.len() < 100 || &page1[..16] != SQLITE_HEADER_MAGIC {
            return None;
        }
        let header_ps = decode_page_size(u32::from(read_u16_be(page1, 16)?))?;
        if header_ps != page_size {
            return None;
        }
        let reserved = usize::from(*page1.get(20)?);
        let usable_size = page_size.checked_sub(reserved).filter(|&u| u >= 480)?;
        let header_pages = read_u32_be(page1, 28)?;
        let effective_pages = if let Some(committed) = wal_pages {
            committed
        } else if header_pages > 0 {
            header_pages
        } else {
            u32::try_from(db.len() / page_size).ok()?
        };
        if effective_pages == 0 {
            return None;
        }
        let encoding = match read_u32_be(page1, 56)? {
            0 | 1 => TextEncoding::Utf8,
            2 => TextEncoding::Utf16Le,
            3 => TextEncoding::Utf16Be,
            _ => return None,
        };
        snap.usable_size = usable_size;
        snap.effective_pages = effective_pages;
        snap.encoding = encoding;
        Some(snap)
    }

    fn page(&self, pgno: u32) -> Option<&'a [u8]> {
        if pgno == 0 || pgno > self.effective_pages {
            return None;
        }
        if let Some(frame) = self.committed_wal.get(&pgno) {
            return Some(*frame);
        }
        let idx = usize::try_from(pgno - 1).ok()?;
        let start = idx.checked_mul(self.page_size)?;
        let end = start.checked_add(self.page_size)?;
        self.db.get(start..end)
    }
}

/// Scans `table_name` in `db` + optional `wal`, collecting values returned by `on_row`.
///
/// Returns `None` if `db`/`wal` is not a readable SQLite 3 database or `table_name` does not exist
/// in `sqlite_schema`.
#[must_use]
pub fn scan_table<R>(
    db: &[u8],
    wal: Option<&[u8]>,
    table_name: &str,
    mut on_row: impl FnMut(&TableSchema, &[SqlValue<'_>]) -> Option<R>,
) -> Option<Vec<R>> {
    let snap = Snapshot::open(db, wal)?;
    let (rootpage, create_sql) = find_table_in_schema(&snap, table_name)?;
    let schema = parse_create_table(&create_sql)?;
    scan_table_rows(&snap, rootpage, &schema, |row| on_row(&schema, row))
}

/// Validates and projects required column indices from `table_name`'s [`TableSchema`] via `prepare`
/// before scanning rows, returning `None` immediately (even on an empty table) when `prepare`
/// rejects the schema.
#[must_use]
pub fn scan_table_with<P, R>(
    db: &[u8],
    wal: Option<&[u8]>,
    table_name: &str,
    prepare: impl FnOnce(&TableSchema) -> Option<P>,
    mut on_row: impl FnMut(&P, &[SqlValue<'_>]) -> Option<R>,
) -> Option<Vec<R>> {
    let snap = Snapshot::open(db, wal)?;
    let (rootpage, create_sql) = find_table_in_schema(&snap, table_name)?;
    let schema = parse_create_table(&create_sql)?;
    let proj = prepare(&schema)?;
    scan_table_rows(&snap, rootpage, &schema, |row| on_row(&proj, row))
}

fn borrow_default<'a>(v: &'a SqlValue<'static>) -> SqlValue<'a> {
    match v {
        SqlValue::Null => SqlValue::Null,
        SqlValue::Int(n) => SqlValue::Int(*n),
        SqlValue::Float(f) => SqlValue::Float(*f),
        SqlValue::Text(s) => SqlValue::Text(Cow::Borrowed(s.as_ref())),
        SqlValue::Blob(b) => SqlValue::Blob(Cow::Borrowed(b.as_ref())),
    }
}

fn materialize_row<'p>(
    rowid: Option<i64>,
    record_bytes: &'p [u8],
    encoding: TextEncoding,
    schema: &'p TableSchema,
    disk_buf: &mut Vec<SqlValue<'p>>,
    row_buf: &mut Vec<SqlValue<'p>>,
) -> Option<()> {
    if let Some(perm) = &schema.disk_to_table {
        decode_record_into(record_bytes, encoding, disk_buf)?;
        row_buf.clear();
        row_buf.extend(schema.defaults.iter().map(borrow_default));
        for (disk_idx, val) in disk_buf.drain(..).enumerate() {
            if let Some(&table_idx) = perm.get(disk_idx)
                && let Some(slot) = row_buf.get_mut(table_idx)
            {
                *slot = val;
            }
        }
    } else {
        decode_record_into(record_bytes, encoding, row_buf)?;
        for col_idx in row_buf.len()..schema.columns.len() {
            row_buf.push(
                schema
                    .defaults
                    .get(col_idx)
                    .map_or(SqlValue::Null, borrow_default),
            );
        }
    }
    if let Some(rowid) = rowid
        && let Some(alias) = schema.rowid_alias_col
        && matches!(row_buf.get(alias), Some(SqlValue::Null))
    {
        row_buf[alias] = SqlValue::Int(rowid);
    }
    Some(())
}

fn scan_table_rows<'a, R>(
    snap: &Snapshot<'a>,
    rootpage: u32,
    schema: &'a TableSchema,
    mut on_row: impl FnMut(&[SqlValue<'_>]) -> Option<R>,
) -> Option<Vec<R>> {
    let mut out = Vec::new();
    let mut rows_seen = 0usize;
    let mut row_buf: Vec<SqlValue<'a>> = Vec::with_capacity(schema.columns.len());
    let mut disk_buf: Vec<SqlValue<'a>> = Vec::with_capacity(if schema.disk_to_table.is_some() {
        schema.columns.len()
    } else {
        0
    });
    scan_btree(snap, rootpage, |rowid, payload| {
        rows_seen = rows_seen.saturating_add(1);
        if rows_seen > MAX_VISITED_ROWS {
            return false;
        }
        let item = match payload {
            Cow::Borrowed(record_bytes) => {
                if materialize_row(
                    rowid,
                    record_bytes,
                    snap.encoding,
                    schema,
                    &mut disk_buf,
                    &mut row_buf,
                )
                .is_none()
                {
                    return false;
                }
                on_row(&row_buf)
            }
            Cow::Owned(ovf_bytes) => {
                let mut ovf_disk = Vec::new();
                let mut ovf_row = Vec::with_capacity(schema.columns.len());
                if materialize_row(
                    rowid,
                    &ovf_bytes,
                    snap.encoding,
                    schema,
                    &mut ovf_disk,
                    &mut ovf_row,
                )
                .is_none()
                {
                    return false;
                }
                on_row(&ovf_row)
            }
        };
        if let Some(item) = item {
            if out.len() >= MAX_COLLECTED_ROWS {
                return false;
            }
            out.push(item);
        }
        true
    })?;
    Some(out)
}

fn find_table_in_schema<'a>(snap: &Snapshot<'a>, table_name: &str) -> Option<(u32, String)> {
    let mut found: Option<(u32, String)> = None;
    let mut cols: Vec<SqlValue<'a>> = Vec::with_capacity(5);
    let check = |cols: &[SqlValue<'_>], found: &mut Option<(u32, String)>| {
        let is_table = cols.first().and_then(SqlValue::as_str) == Some("table");
        let matches = cols
            .get(1)
            .and_then(SqlValue::as_str)
            .is_some_and(|n| n.eq_ignore_ascii_case(table_name));
        if is_table && matches {
            let root = match cols.get(3) {
                Some(SqlValue::Int(n)) => u32::try_from(*n).ok().filter(|&p| p >= 2),
                _ => None,
            };
            let sql = cols.get(4).and_then(SqlValue::as_str).map(str::to_owned);
            if let (Some(root), Some(sql)) = (root, sql) {
                *found = Some((root, sql));
            }
        }
    };
    scan_btree(snap, 1, |_rowid, payload| match payload {
        Cow::Borrowed(record_bytes) => {
            if decode_record_into(record_bytes, snap.encoding, &mut cols).is_none() {
                return false;
            }
            check(&cols, &mut found);
            true
        }
        Cow::Owned(ovf_bytes) => {
            let mut ovf_cols = Vec::with_capacity(5);
            if decode_record_into(&ovf_bytes, snap.encoding, &mut ovf_cols).is_none() {
                return false;
            }
            check(&ovf_cols, &mut found);
            true
        }
    })?;
    found
}

/// Iterative B-tree scan supporting both rowid tables (`0x0d`/`0x05`) and `WITHOUT ROWID` tables (`0x0a`/`0x02`).
fn scan_btree<'a>(
    snap: &Snapshot<'a>,
    root_pgno: u32,
    mut visit: impl FnMut(Option<i64>, Cow<'a, [u8]>) -> bool,
) -> Option<()> {
    enum Step {
        Page(u32),
        InlineCell { pgno: u32, cell_offset: usize },
    }

    let mut visited = HashSet::new();
    let mut stack = vec![Step::Page(root_pgno)];

    while let Some(step) = stack.pop() {
        match step {
            Step::InlineCell { pgno, cell_offset } => {
                let page = snap.page(pgno)?;
                let usable = page.get(..snap.usable_size)?;
                let payload = read_index_cell_payload(snap, usable, cell_offset)?;
                if !visit(None, payload) {
                    return None;
                }
            }
            Step::Page(pgno) => {
                if !visited.insert(pgno) {
                    return None;
                }
                let page = snap.page(pgno)?;
                let usable = page.get(..snap.usable_size)?;
                let hdr = if pgno == 1 { 100 } else { 0 };
                let page_type = *usable.get(hdr)?;
                let cell_count = usize::from(read_u16_be(usable, hdr.checked_add(3)?)?);
                let ptr_bytes = cell_count.checked_mul(2)?;

                match page_type {
                    // Table B-tree leaf page.
                    0x0d => {
                        let ptrs_start = hdr.checked_add(8)?;
                        let cells_floor = ptrs_start.checked_add(ptr_bytes)?;
                        if cells_floor > usable.len() {
                            return None;
                        }
                        for i in 0..cell_count {
                            let ptr_off = ptrs_start.checked_add(i.checked_mul(2)?)?;
                            let cell_off = usize::from(read_u16_be(usable, ptr_off)?);
                            if cell_off < cells_floor {
                                return None;
                            }
                            let (rowid, payload) = read_table_leaf_cell(snap, usable, cell_off)?;
                            if !visit(Some(rowid), payload) {
                                return None;
                            }
                        }
                    }
                    // Table B-tree interior page.
                    0x05 => {
                        let right_child = read_u32_be(usable, hdr.checked_add(8)?)?;
                        let ptrs_start = hdr.checked_add(12)?;
                        let cells_floor = ptrs_start.checked_add(ptr_bytes)?;
                        if cells_floor > usable.len() {
                            return None;
                        }
                        stack.push(Step::Page(right_child));
                        for i in (0..cell_count).rev() {
                            let ptr_off = ptrs_start.checked_add(i.checked_mul(2)?)?;
                            let cell_off = usize::from(read_u16_be(usable, ptr_off)?);
                            if cell_off < cells_floor {
                                return None;
                            }
                            let left_child = read_u32_be(usable, cell_off)?;
                            stack.push(Step::Page(left_child));
                        }
                    }
                    // Index / WITHOUT ROWID B-tree leaf page.
                    0x0a => {
                        let ptrs_start = hdr.checked_add(8)?;
                        let cells_floor = ptrs_start.checked_add(ptr_bytes)?;
                        if cells_floor > usable.len() {
                            return None;
                        }
                        for i in 0..cell_count {
                            let ptr_off = ptrs_start.checked_add(i.checked_mul(2)?)?;
                            let cell_off = usize::from(read_u16_be(usable, ptr_off)?);
                            if cell_off < cells_floor {
                                return None;
                            }
                            let payload = read_index_cell_payload(snap, usable, cell_off)?;
                            if !visit(None, payload) {
                                return None;
                            }
                        }
                    }
                    // Index / WITHOUT ROWID B-tree interior page.
                    0x02 => {
                        let right_child = read_u32_be(usable, hdr.checked_add(8)?)?;
                        let ptrs_start = hdr.checked_add(12)?;
                        let cells_floor = ptrs_start.checked_add(ptr_bytes)?;
                        if cells_floor > usable.len() {
                            return None;
                        }
                        stack.push(Step::Page(right_child));
                        for i in (0..cell_count).rev() {
                            let ptr_off = ptrs_start.checked_add(i.checked_mul(2)?)?;
                            let cell_off = usize::from(read_u16_be(usable, ptr_off)?);
                            if cell_off < cells_floor {
                                return None;
                            }
                            let left_child = read_u32_be(usable, cell_off)?;
                            stack.push(Step::InlineCell {
                                pgno,
                                cell_offset: cell_off.checked_add(4)?,
                            });
                            stack.push(Step::Page(left_child));
                        }
                    }
                    _ => return None,
                }
            }
        }
    }
    Some(())
}

fn read_table_leaf_cell<'a>(
    snap: &Snapshot<'a>,
    usable: &'a [u8],
    cell_offset: usize,
) -> Option<(i64, Cow<'a, [u8]>)> {
    let tail = usable.get(cell_offset..)?;
    let (payload_u64, n1) = read_varint(tail)?;
    let payload_len = usize::try_from(payload_u64)
        .ok()
        .filter(|&len| len <= MAX_CELL_PAYLOAD_BYTES)?;
    let (rowid_u64, n2) = read_varint(tail.get(n1..)?)?;
    #[expect(
        clippy::cast_possible_wrap,
        reason = "SQLite rowid varints store two's-complement 64-bit signed integers"
    )]
    let rowid = rowid_u64 as i64;
    let body_start = cell_offset.checked_add(n1)?.checked_add(n2)?;
    let u = snap.usable_size;
    let max_local = u.checked_sub(35)?;
    let min_local = ((u.checked_sub(12)?) * 32 / 255).checked_sub(23)?;
    let bytes = assemble_payload(snap, usable, body_start, payload_len, max_local, min_local)?;
    Some((rowid, bytes))
}

fn read_index_cell_payload<'a>(
    snap: &Snapshot<'a>,
    usable: &'a [u8],
    cell_offset: usize,
) -> Option<Cow<'a, [u8]>> {
    let tail = usable.get(cell_offset..)?;
    let (payload_u64, n1) = read_varint(tail)?;
    let payload_len = usize::try_from(payload_u64)
        .ok()
        .filter(|&len| len <= MAX_CELL_PAYLOAD_BYTES)?;
    let body_start = cell_offset.checked_add(n1)?;
    let u = snap.usable_size;
    let max_local = ((u.checked_sub(12)?) * 64 / 255).checked_sub(23)?;
    let min_local = ((u.checked_sub(12)?) * 32 / 255).checked_sub(23)?;
    assemble_payload(snap, usable, body_start, payload_len, max_local, min_local)
}

fn assemble_payload<'a>(
    snap: &Snapshot<'a>,
    usable: &'a [u8],
    body_start: usize,
    payload_len: usize,
    max_local: usize,
    min_local: usize,
) -> Option<Cow<'a, [u8]>> {
    if payload_len <= max_local {
        let end = body_start.checked_add(payload_len)?;
        return Some(Cow::Borrowed(usable.get(body_start..end)?));
    }
    let span = snap.usable_size.checked_sub(4).filter(|&s| s > 0)?;
    let k = min_local.checked_add(payload_len.checked_sub(min_local)? % span)?;
    let local_len = if k <= max_local { k } else { min_local };
    let local_end = body_start.checked_add(local_len)?;
    let mut out = Vec::with_capacity(payload_len.min(snap.usable_size.saturating_mul(2)));
    out.extend_from_slice(usable.get(body_start..local_end)?);

    let mut next_pgno = read_u32_be(usable, local_end)?;
    let mut seen_overflow = HashSet::new();
    while out.len() < payload_len {
        if next_pgno == 0 || !seen_overflow.insert(next_pgno) {
            return None;
        }
        let ovf_page = snap.page(next_pgno)?;
        let ovf_usable = ovf_page.get(..snap.usable_size)?;
        next_pgno = read_u32_be(ovf_usable, 0)?;
        let chunk = ovf_usable.get(4..)?;
        let need = payload_len - out.len();
        let take = need.min(chunk.len());
        out.extend_from_slice(chunk.get(..take)?);
    }
    Some(Cow::Owned(out))
}

#[allow(clippy::type_complexity)]
fn parse_wal(
    wal: &[u8],
    expected_page_size: Option<usize>,
) -> Option<(HashMap<u32, &[u8]>, Option<u32>, Option<usize>)> {
    if wal.len() < WAL_HEADER_LEN {
        return Some((HashMap::new(), None, None));
    }
    let magic = read_u32_be(wal, 0)?;
    let little_endian = match magic {
        WAL_MAGIC_LE => true,
        WAL_MAGIC_BE => false,
        _ => return None,
    };
    let (hdr_s0, hdr_s1) = wal_checksum(&wal[..24], (0, 0), little_endian)?;
    let want_s0 = read_u32_be(wal, 24)?;
    let want_s1 = read_u32_be(wal, 28)?;
    if (hdr_s0, hdr_s1) != (want_s0, want_s1) {
        // An uninitialized or invalidated WAL header holds zero committed frames.
        return Some((HashMap::new(), None, None));
    }
    let wal_ps = decode_page_size(read_u32_be(wal, 8)?)?;
    if let Some(main_ps) = expected_page_size
        && wal_ps != main_ps
    {
        return None;
    }

    let salt1 = read_u32_be(wal, 16)?;
    let salt2 = read_u32_be(wal, 20)?;
    let frame_stride = WAL_FRAME_HEADER_LEN.checked_add(wal_ps)?;

    let mut s0 = hdr_s0;
    let mut s1 = hdr_s1;
    let mut committed: HashMap<u32, &[u8]> = HashMap::new();
    let mut pending: Vec<(u32, &[u8])> = Vec::new();
    let mut committed_db_pages: Option<u32> = None;

    let mut offset = WAL_HEADER_LEN;
    while let Some(frame_end) = offset.checked_add(frame_stride) {
        let Some(frame) = wal.get(offset..frame_end) else {
            break;
        };
        let pgno = read_u32_be(frame, 0)?;
        if pgno == 0 {
            break;
        }
        let db_size_after_commit = read_u32_be(frame, 4)?;
        let f_salt1 = read_u32_be(frame, 8)?;
        let f_salt2 = read_u32_be(frame, 12)?;
        if (f_salt1, f_salt2) != (salt1, salt2) {
            break;
        }
        let expected_s0 = read_u32_be(frame, 16)?;
        let expected_s1 = read_u32_be(frame, 20)?;
        let page_bytes = frame.get(WAL_FRAME_HEADER_LEN..)?;
        (s0, s1) = wal_checksum(&frame[..8], (s0, s1), little_endian)?;
        (s0, s1) = wal_checksum(page_bytes, (s0, s1), little_endian)?;
        if (s0, s1) != (expected_s0, expected_s1) {
            break;
        }
        pending.push((pgno, page_bytes));
        if db_size_after_commit > 0 {
            for (p, bytes) in pending.drain(..) {
                if p <= db_size_after_commit {
                    committed.insert(p, bytes);
                }
            }
            committed.retain(|&p, _| p <= db_size_after_commit);
            committed_db_pages = Some(db_size_after_commit);
        }
        offset = frame_end;
    }

    Some((committed, committed_db_pages, Some(wal_ps)))
}

fn wal_checksum(bytes: &[u8], init: (u32, u32), little_endian: bool) -> Option<(u32, u32)> {
    let (chunks, rem) = bytes.as_chunks::<8>();
    if !rem.is_empty() {
        return None;
    }
    let (mut s0, mut s1) = init;
    for chunk in chunks {
        let b0: [u8; 4] = chunk[0..4].try_into().ok()?;
        let b1: [u8; 4] = chunk[4..8].try_into().ok()?;
        let (x0, x1) = if little_endian {
            (u32::from_le_bytes(b0), u32::from_le_bytes(b1))
        } else {
            (u32::from_be_bytes(b0), u32::from_be_bytes(b1))
        };
        s0 = s0.wrapping_add(x0).wrapping_add(s1);
        s1 = s1.wrapping_add(x1).wrapping_add(s0);
    }
    Some((s0, s1))
}

fn decode_page_size(raw: u32) -> Option<usize> {
    let ps = if raw == 1 {
        65_536
    } else {
        usize::try_from(raw).ok()?
    };
    ((512..=65_536).contains(&ps) && ps.is_power_of_two()).then_some(ps)
}

fn read_u16_be(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_be_bytes(slice))
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(slice))
}

fn read_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut acc = 0u64;
    for i in 0..8 {
        let b = *bytes.get(i)?;
        acc = (acc << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Some((acc, i + 1));
        }
    }
    let b9 = *bytes.get(8)?;
    acc = (acc << 8) | u64::from(b9);
    Some((acc, 9))
}

fn decode_record_into<'a>(
    payload: &'a [u8],
    encoding: TextEncoding,
    values: &mut Vec<SqlValue<'a>>,
) -> Option<()> {
    values.clear();
    let (hdr_len_u64, mut hdr_pos) = read_varint(payload)?;
    let hdr_len = usize::try_from(hdr_len_u64).ok()?;
    if hdr_len < hdr_pos || hdr_len > payload.len() {
        return None;
    }

    let mut body_pos = hdr_len;
    while hdr_pos < hdr_len {
        let (st, used) = read_varint(payload.get(hdr_pos..hdr_len)?)?;
        hdr_pos = hdr_pos.checked_add(used)?;
        let val = match st {
            0 => SqlValue::Null,
            1 => {
                let end = body_pos.checked_add(1)?;
                let b = *payload.get(body_pos)?;
                body_pos = end;
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "SQLite 1-byte integer serial type is two's-complement i8"
                )]
                SqlValue::Int(i64::from(b as i8))
            }
            2 => {
                let end = body_pos.checked_add(2)?;
                let b: [u8; 2] = payload.get(body_pos..end)?.try_into().ok()?;
                body_pos = end;
                SqlValue::Int(i64::from(i16::from_be_bytes(b)))
            }
            3 => {
                let end = body_pos.checked_add(3)?;
                let b = payload.get(body_pos..end)?;
                body_pos = end;
                let sign = if b[0] & 0x80 != 0 { 0xFF } else { 0x00 };
                SqlValue::Int(i64::from(i32::from_be_bytes([sign, b[0], b[1], b[2]])))
            }
            4 => {
                let end = body_pos.checked_add(4)?;
                let b: [u8; 4] = payload.get(body_pos..end)?.try_into().ok()?;
                body_pos = end;
                SqlValue::Int(i64::from(i32::from_be_bytes(b)))
            }
            5 => {
                let end = body_pos.checked_add(6)?;
                let b = payload.get(body_pos..end)?;
                body_pos = end;
                let sign = if b[0] & 0x80 != 0 { 0xFF } else { 0x00 };
                SqlValue::Int(i64::from_be_bytes([
                    sign, sign, b[0], b[1], b[2], b[3], b[4], b[5],
                ]))
            }
            6 => {
                let end = body_pos.checked_add(8)?;
                let b: [u8; 8] = payload.get(body_pos..end)?.try_into().ok()?;
                body_pos = end;
                SqlValue::Int(i64::from_be_bytes(b))
            }
            7 => {
                let end = body_pos.checked_add(8)?;
                let b: [u8; 8] = payload.get(body_pos..end)?.try_into().ok()?;
                body_pos = end;
                let f = f64::from_bits(u64::from_be_bytes(b));
                if f.is_nan() {
                    SqlValue::Null
                } else {
                    SqlValue::Float(f)
                }
            }
            8 => SqlValue::Int(0),
            9 => SqlValue::Int(1),
            10 | 11 => return None,
            n if n >= 12 && n % 2 == 0 => {
                let len = usize::try_from((n - 12) / 2).ok()?;
                let end = body_pos.checked_add(len)?;
                let slice = payload.get(body_pos..end)?;
                body_pos = end;
                SqlValue::Blob(Cow::Borrowed(slice))
            }
            n if n >= 13 => {
                let len = usize::try_from((n - 13) / 2).ok()?;
                let end = body_pos.checked_add(len)?;
                let slice = payload.get(body_pos..end)?;
                body_pos = end;
                SqlValue::Text(decode_text(slice, encoding)?)
            }
            _ => return None,
        };
        values.push(val);
    }
    Some(())
}

fn decode_text<'a>(bytes: &'a [u8], encoding: TextEncoding) -> Option<Cow<'a, str>> {
    match encoding {
        TextEncoding::Utf8 => Some(String::from_utf8_lossy(bytes)),
        TextEncoding::Utf16Le => {
            let (chunks, rem) = bytes.as_chunks::<2>();
            if !rem.is_empty() {
                return None;
            }
            let units: Vec<u16> = chunks.iter().copied().map(u16::from_le_bytes).collect();
            Some(Cow::Owned(String::from_utf16_lossy(&units)))
        }
        TextEncoding::Utf16Be => {
            let (chunks, rem) = bytes.as_chunks::<2>();
            if !rem.is_empty() {
                return None;
            }
            let units: Vec<u16> = chunks.iter().copied().map(u16::from_be_bytes).collect();
            Some(Cow::Owned(String::from_utf16_lossy(&units)))
        }
    }
}

/// Extracts ordered column names (and any `INTEGER PRIMARY KEY` rowid alias) from a SQLite
/// `CREATE TABLE` DDL string stored in `sqlite_schema.sql`.
pub(crate) fn parse_create_table(sql: &str) -> Option<TableSchema> {
    let (body, tail) = extract_paren_body(sql)?;
    let without_rowid = contains_without_rowid(tail);
    let mut columns = Vec::new();
    let mut defaults = Vec::new();
    let mut rowid_alias_col = None;
    let mut table_pk_override = false;
    let mut col_pk_indices = Vec::new();
    let mut table_pk_names = Vec::new();

    for item in split_top_level_commas(body) {
        let tokens = tokenize_sql_words(item);
        let Some(first) = tokens.first() else {
            continue;
        };
        if !first.quoted
            && matches!(
                first.text.to_ascii_uppercase().as_str(),
                "CONSTRAINT" | "PRIMARY" | "UNIQUE" | "CHECK" | "FOREIGN"
            )
        {
            if has_primary_key_clause(&tokens) {
                table_pk_override = true;
                if without_rowid {
                    table_pk_names = extract_primary_key_columns(item);
                }
            }
            continue;
        }
        if is_virtual_generated_column(&tokens[1..]) {
            continue;
        }
        let col_idx = columns.len();
        columns.push(first.text.clone());
        defaults.push(extract_column_default(item));

        if has_primary_key_clause(&tokens[1..]) {
            col_pk_indices.push(col_idx);
        }
        if !without_rowid && is_integer_primary_key(&tokens[1..]) {
            rowid_alias_col = Some(col_idx);
        }
    }

    if columns.is_empty() {
        return None;
    }
    if table_pk_override || without_rowid {
        rowid_alias_col = None;
    }
    let disk_to_table = if without_rowid {
        build_without_rowid_perm(
            &columns,
            table_pk_override,
            &table_pk_names,
            &col_pk_indices,
        )?
    } else {
        None
    };
    Some(TableSchema {
        columns,
        defaults,
        rowid_alias_col,
        disk_to_table,
    })
}

fn has_primary_key_clause(tokens: &[SqlToken]) -> bool {
    tokens.windows(2).any(|w| {
        !w[0].quoted
            && !w[1].quoted
            && w[0].text.eq_ignore_ascii_case("PRIMARY")
            && w[1].text.eq_ignore_ascii_case("KEY")
    })
}

fn extract_primary_key_columns(item: &str) -> Vec<String> {
    let bytes = item.as_bytes();
    let mut i = 0usize;
    let mut depth = 0usize;
    let mut saw_primary = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_line_comment(bytes, i + 2),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i + 2),
            b'\'' | b'"' | b'`' => {
                i = skip_quoted(bytes, i, b);
                if depth == 0 {
                    saw_primary = false;
                }
            }
            b'[' => {
                i = skip_bracket(bytes, i);
                if depth == 0 {
                    saw_primary = false;
                }
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ if depth == 0 && is_ident_byte(b) => {
                let start = i;
                while i < bytes.len() && is_ident_byte(bytes[i]) {
                    i += 1;
                }
                let word = item.get(start..i).unwrap_or("");
                if saw_primary && word.eq_ignore_ascii_case("KEY") {
                    let Some((pk_body, _)) = item.get(i..).and_then(extract_paren_body) else {
                        return Vec::new();
                    };
                    return split_top_level_commas(pk_body)
                        .into_iter()
                        .filter_map(|part| tokenize_sql_words(part).into_iter().next())
                        .map(|tok| tok.text)
                        .collect();
                }
                saw_primary = word.eq_ignore_ascii_case("PRIMARY");
            }
            _ => {
                if depth == 0 {
                    saw_primary = false;
                }
                i += 1;
            }
        }
    }
    Vec::new()
}

fn build_without_rowid_perm(
    columns: &[String],
    table_pk_override: bool,
    table_pk_names: &[String],
    col_pk_indices: &[usize],
) -> Option<Option<Vec<usize>>> {
    if table_pk_override && !col_pk_indices.is_empty() {
        return None;
    }
    let mut perm = Vec::with_capacity(columns.len());
    if table_pk_override {
        if table_pk_names.is_empty() {
            return None;
        }
        for pk_name in table_pk_names {
            let idx = columns
                .iter()
                .position(|col| col.eq_ignore_ascii_case(pk_name))?;
            if !perm.contains(&idx) {
                perm.push(idx);
            }
        }
    } else {
        if col_pk_indices.len() != 1 {
            return None;
        }
        perm.push(col_pk_indices[0]);
    }
    if perm.is_empty() {
        return None;
    }
    for idx in 0..columns.len() {
        if !perm.contains(&idx) {
            perm.push(idx);
        }
    }
    Some(
        perm.iter()
            .enumerate()
            .any(|(disk_idx, &col_idx)| disk_idx != col_idx)
            .then_some(perm),
    )
}

fn extract_column_default(item: &str) -> SqlValue<'static> {
    let rest = skip_ws_and_comments(item);
    let bytes = rest.as_bytes();
    let Some(&first) = bytes.first() else {
        return SqlValue::Null;
    };
    let mut i = match first {
        b'\'' | b'"' | b'`' => skip_quoted(bytes, 0, first),
        b'[' => skip_bracket(bytes, 0),
        _ if is_ident_byte(first) => {
            let mut end = 1;
            while end < bytes.len() && is_ident_byte(bytes[end]) {
                end += 1;
            }
            end
        }
        _ => return SqlValue::Null,
    };
    let mut depth = 0usize;
    let mut prev_word_is_set = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_line_comment(bytes, i + 2),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i + 2),
            b'\'' | b'"' | b'`' => {
                i = skip_quoted(bytes, i, b);
                if depth == 0 {
                    prev_word_is_set = false;
                }
            }
            b'[' => {
                i = skip_bracket(bytes, i);
                if depth == 0 {
                    prev_word_is_set = false;
                }
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ if depth == 0 && is_ident_byte(b) => {
                let start = i;
                while i < bytes.len() && is_ident_byte(bytes[i]) {
                    i += 1;
                }
                let word = rest.get(start..i).unwrap_or("");
                if !prev_word_is_set && word.eq_ignore_ascii_case("DEFAULT") {
                    return rest
                        .get(i..)
                        .and_then(|tail| parse_sql_default_literal(tail, false))
                        .unwrap_or(SqlValue::Null);
                }
                prev_word_is_set = word.eq_ignore_ascii_case("SET");
            }
            _ => {
                if depth == 0 {
                    prev_word_is_set = false;
                }
                i += 1;
            }
        }
    }
    SqlValue::Null
}

fn parse_sql_default_literal(src: &str, mut in_parens: bool) -> Option<SqlValue<'static>> {
    let mut cur = skip_ws_and_comments(src);
    for _ in 0..8 {
        if !cur.starts_with('(') {
            break;
        }
        let (inner, after) = extract_paren_body(cur)?;
        if in_parens && !skip_ws_and_comments(after).is_empty() {
            return None;
        }
        cur = skip_ws_and_comments(inner);
        in_parens = true;
    }
    let bytes = cur.as_bytes();
    let &first = bytes.first()?;
    let (val, rem) = if first == b'\'' {
        let end = skip_quoted(bytes, 0, b'\'');
        if end < 2 || bytes.get(end - 1) != Some(&b'\'') {
            return None;
        }
        let inner = cur.get(1..end - 1)?;
        (
            SqlValue::Text(Cow::Owned(inner.replace("''", "'"))),
            cur.get(end..)?,
        )
    } else if matches!(first, b'x' | b'X') && bytes.get(1) == Some(&b'\'') {
        let end = skip_quoted(bytes, 1, b'\'');
        if end < 3 || bytes.get(end - 1) != Some(&b'\'') {
            return None;
        }
        let hex = cur.get(2..end - 1)?;
        (
            SqlValue::Blob(Cow::Owned(decode_hex_blob(hex)?)),
            cur.get(end..)?,
        )
    } else if matches!(first, b'+' | b'-') || first.is_ascii_digit() || first == b'.' {
        let (sign, num_src) = if matches!(first, b'+' | b'-') {
            (first == b'-', skip_ws_and_comments(cur.get(1..)?))
        } else {
            (false, cur)
        };
        let nb = num_src.as_bytes();
        let mut end = 0usize;
        while end < nb.len() {
            let c = nb[end];
            let exp_sign = matches!(c, b'+' | b'-')
                && end > 0
                && matches!(nb[end - 1], b'e' | b'E')
                && (nb[0].is_ascii_digit() || nb[0] == b'.');
            if c.is_ascii_alphanumeric() || c == b'.' || exp_sign {
                end += 1;
            } else {
                break;
            }
        }
        let tok = num_src.get(..end).filter(|t| !t.is_empty())?;
        let parsed = if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
            let signed = if sign {
                format!("-{hex}")
            } else {
                hex.to_owned()
            };
            SqlValue::Int(i64::from_str_radix(&signed, 16).ok()?)
        } else {
            let signed = if sign {
                format!("-{tok}")
            } else {
                tok.to_owned()
            };
            if let Ok(n) = signed.parse::<i64>() {
                SqlValue::Int(n)
            } else {
                let f = signed.parse::<f64>().ok().filter(|v| v.is_finite())?;
                SqlValue::Float(f)
            }
        };
        (parsed, num_src.get(end..)?)
    } else if is_ident_byte(first) {
        let mut end = 1usize;
        while end < bytes.len() && is_ident_byte(bytes[end]) {
            end += 1;
        }
        let word = cur.get(..end)?;
        let val = if word.eq_ignore_ascii_case("NULL") {
            SqlValue::Null
        } else if word.eq_ignore_ascii_case("TRUE") {
            SqlValue::Int(1)
        } else if word.eq_ignore_ascii_case("FALSE") {
            SqlValue::Int(0)
        } else {
            return None;
        };
        (val, cur.get(end..)?)
    } else {
        return None;
    };
    if in_parens && !skip_ws_and_comments(rem).is_empty() {
        return None;
    }
    Some(val)
}

fn skip_ws_and_comments(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'-') {
            i = skip_line_comment(bytes, i + 2);
        } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = skip_block_comment(bytes, i + 2);
        } else {
            break;
        }
    }
    s.get(i..).unwrap_or("")
}

fn decode_hex_blob(hex: &str) -> Option<Vec<u8>> {
    let (pairs, rem) = hex.as_bytes().as_chunks::<2>();
    if !rem.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(pairs.len());
    for &[hi, lo] in pairs {
        let h = char::from(hi).to_digit(16)?;
        let l = char::from(lo).to_digit(16)?;
        let byte = u8::try_from((h << 4) | l).ok()?;
        out.push(byte);
    }
    Some(out)
}

fn contains_without_rowid(tail: &str) -> bool {
    let words = tokenize_sql_words(tail);
    words.windows(2).any(|w| {
        !w[0].quoted
            && !w[1].quoted
            && w[0].text.eq_ignore_ascii_case("WITHOUT")
            && w[1].text.eq_ignore_ascii_case("ROWID")
    })
}

fn is_virtual_generated_column(rest: &[SqlToken]) -> bool {
    let Some(as_idx) = rest.iter().enumerate().find_map(|(idx, tok)| {
        let after_ref = idx > 0
            && !rest[idx - 1].quoted
            && rest[idx - 1].text.eq_ignore_ascii_case("REFERENCES");
        (!after_ref && !tok.quoted && tok.followed_by_paren && tok.text.eq_ignore_ascii_case("AS"))
            .then_some(idx)
    }) else {
        return false;
    };
    !rest[as_idx + 1..]
        .iter()
        .any(|tok| !tok.quoted && tok.text.eq_ignore_ascii_case("STORED"))
}

fn is_column_constraint_starter(tok: &SqlToken) -> bool {
    !tok.quoted
        && matches!(
            tok.text.to_ascii_uppercase().as_str(),
            "CONSTRAINT"
                | "PRIMARY"
                | "NOT"
                | "NULL"
                | "UNIQUE"
                | "CHECK"
                | "DEFAULT"
                | "COLLATE"
                | "REFERENCES"
                | "GENERATED"
                | "AS"
        )
}

fn is_integer_primary_key(rest: &[SqlToken]) -> bool {
    let Some(ty) = rest.first() else {
        return false;
    };
    // SQLite's `sqlite3AddPrimaryKey` aliases rowid ONLY when the declared type is the exact
    // single token `INTEGER` (no `(size)` modifier and no multi-word typename like
    // `INTEGER UNSIGNED` preceding the first column constraint).
    if !ty.text.eq_ignore_ascii_case("INTEGER")
        || ty.followed_by_paren
        || !rest.get(1).is_some_and(is_column_constraint_starter)
    {
        return false;
    }
    for i in 1..rest.len().saturating_sub(1) {
        if !rest[i].quoted
            && !rest[i + 1].quoted
            && rest[i].text.eq_ignore_ascii_case("PRIMARY")
            && rest[i + 1].text.eq_ignore_ascii_case("KEY")
        {
            // `INTEGER PRIMARY KEY DESC` is NOT a rowid alias in SQLite (historical quirk).
            let desc = rest
                .get(i + 2)
                .is_some_and(|t| !t.quoted && t.text.eq_ignore_ascii_case("DESC"));
            return !desc;
        }
    }
    false
}

#[derive(Debug)]
struct SqlToken {
    text: String,
    quoted: bool,
    followed_by_paren: bool,
}

fn extract_paren_body(sql: &str) -> Option<(&str, &str)> {
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    let mut open_at = None;
    let mut depth = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => {
                i = skip_quoted(bytes, i, bytes[i]);
            }
            b'[' => {
                i = skip_bracket(bytes, i);
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = skip_line_comment(bytes, i + 2);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = skip_block_comment(bytes, i + 2);
            }
            b'(' => {
                if depth == 0 {
                    open_at = Some(i + 1);
                }
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let start = open_at?;
                    return Some((sql.get(start..i)?, sql.get(i + 1..)?));
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

fn split_top_level_commas(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(bytes, i, bytes[i]),
            b'[' => i = skip_bracket(bytes, i),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_line_comment(bytes, i + 2),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i + 2),
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b',' if depth == 0 => {
                if let Some(piece) = body.get(start..i) {
                    parts.push(piece);
                }
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    if let Some(tail) = body.get(start..) {
        parts.push(tail);
    }
    parts
}

fn tokenize_sql_words(item: &str) -> Vec<SqlToken> {
    let bytes = item.as_bytes();
    let mut out: Vec<SqlToken> = Vec::new();
    let mut i = 0usize;
    let mut depth = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_line_comment(bytes, i + 2),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i + 2),
            b'(' => {
                if depth == 0
                    && let Some(last) = out.last_mut()
                {
                    last.followed_by_paren = true;
                }
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'\'' => {
                let end = skip_quoted(bytes, i, b'\'');
                if depth == 0
                    && out.is_empty()
                    && end >= i + 2
                    && bytes.get(end - 1) == Some(&b'\'')
                    && let Some(inner) = item.get(i + 1..end - 1)
                {
                    out.push(SqlToken {
                        text: inner.replace("''", "'"),
                        quoted: true,
                        followed_by_paren: false,
                    });
                }
                i = end;
            }
            b'"' | b'`' => {
                let end = skip_quoted(bytes, i, b);
                if depth == 0
                    && end >= i + 2
                    && bytes.get(end - 1) == Some(&b)
                    && let Some(inner) = item.get(i + 1..end - 1)
                {
                    let unescaped = if b == b'"' {
                        inner.replace("\"\"", "\"")
                    } else {
                        inner.replace("``", "`")
                    };
                    out.push(SqlToken {
                        text: unescaped,
                        quoted: true,
                        followed_by_paren: false,
                    });
                }
                i = end;
            }
            b'[' => {
                let end = skip_bracket(bytes, i);
                if depth == 0
                    && end >= i + 2
                    && bytes.get(end - 1) == Some(&b']')
                    && let Some(inner) = item.get(i + 1..end - 1)
                {
                    out.push(SqlToken {
                        text: inner.to_owned(),
                        quoted: true,
                        followed_by_paren: false,
                    });
                }
                i = end;
            }
            _ if depth > 0 => {
                i += 1;
            }
            _ if is_ident_byte(b) => {
                let start = i;
                while i < bytes.len() && is_ident_byte(bytes[i]) {
                    i += 1;
                }
                if let Some(word) = item.get(start..i) {
                    out.push(SqlToken {
                        text: word.to_owned(),
                        quoted: false,
                        followed_by_paren: false,
                    });
                }
            }
            _ => i += 1,
        }
    }
    out
}

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

fn skip_quoted(bytes: &[u8], open: usize, quote: u8) -> usize {
    let mut i = open + 1;
    while i < bytes.len() {
        if bytes[i] == quote {
            if bytes.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_bracket(bytes: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    while i < bytes.len() {
        if bytes[i] == b']' {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_line_comment(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

fn skip_block_comment(bytes: &[u8], mut i: usize) -> usize {
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return i + 2;
        }
        i += 1;
    }
    bytes.len()
}

#[cfg(test)]
#[path = "sqlite_read_tests.rs"]
mod tests;
