// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unit tests for the pure-Rust SQLite 3 B-tree and WAL snapshot reader.

use super::*;

#[test]
fn create_table_ddl_extracts_columns_and_rowid_alias() {
    let muse = parse_create_table(
        "CREATE TABLE goals (session_id TEXT PRIMARY KEY, goal_id TEXT NOT NULL, \
         objective TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('active','paused')), \
         percent_complete INTEGER NOT NULL)",
    )
    .expect("muse ddl");
    assert_eq!(
        muse.columns,
        [
            "session_id",
            "goal_id",
            "objective",
            "status",
            "percent_complete"
        ]
    );
    assert_eq!(muse.rowid_alias_col, None);

    let aliased = parse_create_table(
        r#"CREATE TABLE "items" (
            id INTEGER PRIMARY KEY AUTOINCREMENT, -- rowid alias
            "label""x" TEXT DEFAULT 'a,b',
            [count] INT,
            CONSTRAINT ck CHECK (id > 0)
        )"#,
    )
    .expect("aliased ddl");
    assert_eq!(aliased.columns, ["id", "label\"x", "count"]);
    assert_eq!(aliased.rowid_alias_col, Some(0));
    assert_eq!(
        aliased.defaults,
        [
            SqlValue::Null,
            SqlValue::Text("a,b".to_owned()),
            SqlValue::Null
        ]
    );

    let composite = parse_create_table(
        "CREATE TABLE pairs (id INTEGER, part INTEGER, PRIMARY KEY (id, part)) WITHOUT ROWID",
    )
    .expect("without rowid");
    assert_eq!(composite.columns, ["id", "part"]);
    assert_eq!(composite.rowid_alias_col, None);
    assert_eq!(composite.disk_to_table, None);

    let reordered_wr = parse_create_table(
        r#"CREATE TABLE perm_kv (
            payload TEXT,
            "k1" TEXT,
            k2 INT,
            extra BLOB DEFAULT X'4142',
            CONSTRAINT "PRIMARY KEY (fake)" PRIMARY KEY (k2 DESC, "k1" COLLATE NOCASE)
        ) WITHOUT ROWID"#,
    )
    .expect("reordered without rowid");
    assert_eq!(reordered_wr.columns, ["payload", "k1", "k2", "extra"]);
    assert_eq!(reordered_wr.disk_to_table, Some(vec![2, 1, 0, 3]));
    assert_eq!(reordered_wr.defaults[3], SqlValue::Blob(b"AB".to_vec()));

    let col_wr =
        parse_create_table("CREATE TABLE col_wr (val TEXT, k TEXT PRIMARY KEY) WITHOUT ROWID")
            .expect("col pk without rowid");
    assert_eq!(col_wr.disk_to_table, Some(vec![1, 0]));

    let fk_and_defs = parse_create_table(
        r#"CREATE TABLE defs (
            "default" INT REFERENCES parent(id) ON DELETE SET DEFAULT,
            retries INT DEFAULT (-7) NOT NULL,
            enabled BOOLEAN DEFAULT TRUE,
            ratio REAL DEFAULT 1.5
        )"#,
    )
    .expect("fk and defaults");
    assert_eq!(
        fk_and_defs.defaults,
        [
            SqlValue::Null,
            SqlValue::Int(-7),
            SqlValue::Int(1),
            SqlValue::Float(1.5),
        ]
    );

    let quoted_int = parse_create_table(
        r#"CREATE TABLE docs (id "INTEGER" NOT NULL PRIMARY KEY, body TEXT, CONSTRAINT primary_note CHECK (id > 0))"#,
    )
    .expect("quoted integer pk");
    assert_eq!(quoted_int.columns, ["id", "body"]);
    assert_eq!(quoted_int.rowid_alias_col, Some(0));

    let desc_pk =
        parse_create_table("CREATE TABLE rev (id INTEGER PRIMARY KEY DESC, body TEXT)").unwrap();
    assert_eq!(desc_pk.rowid_alias_col, None);

    let sized_int =
        parse_create_table("CREATE TABLE sized (id INTEGER(8) PRIMARY KEY, body TEXT)").unwrap();
    assert_eq!(sized_int.rowid_alias_col, None);

    let multi_word_int =
        parse_create_table("CREATE TABLE uns (id INTEGER UNSIGNED PRIMARY KEY, body TEXT)")
            .unwrap();
    assert_eq!(multi_word_int.rowid_alias_col, None);

    let table_pk = parse_create_table(
        "CREATE TABLE overridden (id INTEGER, slug TEXT, CONSTRAINT pk PRIMARY KEY (id))",
    )
    .unwrap();
    assert_eq!(table_pk.rowid_alias_col, None);

    // Malformed `WITHOUT ROWID` schemas without a valid PRIMARY KEY must return `None` rather than
    // silently falling back to identity column order.
    for bad_wr in [
        "CREATE TABLE no_pk (a TEXT, b INT) WITHOUT ROWID",
        "CREATE TABLE empty_pk (a TEXT, PRIMARY KEY ()) WITHOUT ROWID",
        "CREATE TABLE missing_pk (a TEXT, PRIMARY KEY (nope)) WITHOUT ROWID",
        "CREATE TABLE two_col_pk (a TEXT PRIMARY KEY, b TEXT PRIMARY KEY) WITHOUT ROWID",
        "CREATE TABLE mixed_pk (a TEXT PRIMARY KEY, b TEXT, PRIMARY KEY (b)) WITHOUT ROWID",
    ] {
        assert!(
            parse_create_table(bad_wr).is_none(),
            "{bad_wr} must be rejected"
        );
    }

    // IEEE-754 NaN in serial type 7 decodes to `SqlValue::Null`, matching SQLite's `vdbeaux.c`.
    let mut nan_record = vec![2u8, 7u8];
    nan_record.extend_from_slice(&f64::NAN.to_bits().to_be_bytes());
    assert_eq!(
        decode_record(&nan_record, TextEncoding::Utf8),
        Some(vec![SqlValue::Null])
    );
}

#[test]
fn malformed_or_cyclic_pages_return_none_without_panicking() {
    assert!(scan_table(b"", None, "goals", |_, _| Some(())).is_none());
    assert!(scan_table(b"not sqlite at all", None, "goals", |_, _| Some(())).is_none());

    // Build a synthetic 512-byte 2-page DB where page 1's interior B-tree points back to page 1.
    let mut db = vec![0u8; 1024];
    db[..16].copy_from_slice(SQLITE_HEADER_MAGIC);
    db[16..18].copy_from_slice(&512u16.to_be_bytes());
    db[28..32].copy_from_slice(&2u32.to_be_bytes());
    db[56..60].copy_from_slice(&1u32.to_be_bytes());
    // Page 1 B-tree header at offset 100: interior table page (0x05) with right_child = 1 (cycle!).
    db[100] = 0x05;
    db[103..105].copy_from_slice(&0u16.to_be_bytes());
    db[108..112].copy_from_slice(&1u32.to_be_bytes());
    assert!(scan_table(&db, None, "goals", |_, _| Some(())).is_none());

    // Build a synthetic 512-byte 1-page DB whose leaf cell claims a 4 MiB overflow payload
    // pointing to non-existent overflow page 99: fails cleanly without huge pre-allocations.
    let mut ovf = vec![0u8; 512];
    ovf[..16].copy_from_slice(SQLITE_HEADER_MAGIC);
    ovf[16..18].copy_from_slice(&512u16.to_be_bytes());
    ovf[28..32].copy_from_slice(&1u32.to_be_bytes());
    ovf[56..60].copy_from_slice(&1u32.to_be_bytes());
    ovf[100] = 0x0d; // leaf table B-tree
    ovf[103..105].copy_from_slice(&1u16.to_be_bytes()); // 1 cell
    ovf[108..110].copy_from_slice(&200u16.to_be_bytes()); // cell at offset 200
    // Varint for 4_194_304 (0x40_0000): 0x81, 0x80, 0x80, 0x00, rowid = 1.
    ovf[200..205].copy_from_slice(&[0x81, 0x80, 0x80, 0x00, 0x01]);
    assert!(scan_table(&ovf, None, "goals", |_, _| Some(())).is_none());

    // A cell pointer overlapping the DB header or B-tree cell-pointer array (`cell_off < 110`)
    // must be rejected rather than dereferenced as a cell.
    let mut overlap = vec![0u8; 512];
    overlap[..16].copy_from_slice(SQLITE_HEADER_MAGIC);
    overlap[16..18].copy_from_slice(&512u16.to_be_bytes());
    overlap[28..32].copy_from_slice(&1u32.to_be_bytes());
    overlap[56..60].copy_from_slice(&1u32.to_be_bytes());
    overlap[100] = 0x0d;
    overlap[103..105].copy_from_slice(&1u16.to_be_bytes());
    overlap[108..110].copy_from_slice(&0u16.to_be_bytes()); // points into "SQLite format 3\0"!
    assert!(scan_table(&overlap, None, "goals", |_, _| Some(())).is_none());

    // A corrupt `sqlite_schema` row naming `rootpage = 1` (page 1 is `sqlite_schema` itself)
    // must not scan `sqlite_schema` metadata rows as user-table rows.
    let ddl = b"CREATE TABLE goals (t TEXT, n TEXT, tn TEXT, r INT, s TEXT)";
    let ddl_serial = u8::try_from(ddl.len() * 2 + 13).expect("ddl serial");
    let mut rec = vec![
        6u8,        // header len
        23u8,       // "table" (len 5 -> 5*2+13 = 23)
        23u8,       // "goals" (len 5 -> 23)
        23u8,       // "goals" (len 5 -> 23)
        9u8,        // constant integer 1 -> rootpage = 1 (corrupt!)
        ddl_serial, // sql text
    ];
    rec.extend_from_slice(b"tablegoalsgoals");
    rec.extend_from_slice(ddl);
    let mut root1 = vec![0u8; 512];
    root1[..16].copy_from_slice(SQLITE_HEADER_MAGIC);
    root1[16..18].copy_from_slice(&512u16.to_be_bytes());
    root1[28..32].copy_from_slice(&1u32.to_be_bytes());
    root1[56..60].copy_from_slice(&1u32.to_be_bytes());
    root1[100] = 0x0d;
    root1[103..105].copy_from_slice(&1u16.to_be_bytes());
    root1[108..110].copy_from_slice(&200u16.to_be_bytes());
    root1[200] = u8::try_from(rec.len()).expect("payload len");
    root1[201] = 1; // rowid = 1
    root1[202..202 + rec.len()].copy_from_slice(&rec);
    assert!(
        scan_table(&root1, None, "goals", |_, _| Some(())).is_none(),
        "rootpage = 1 user table in sqlite_schema must be rejected"
    );
}

#[cfg(target_os = "macos")]
mod live_sqlite {
    use super::*;

    fn run_sqlite(db: &std::path::Path, sql: &str) {
        let out = crate::deadline::output_until(
            crate::spawnfd::command("/usr/bin/sqlite3").arg(db).arg(sql),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .expect("sqlite3");
        assert!(out.status.success(), "{out:?}");
    }

    #[test]
    fn reads_uncheckpointed_wal_overflow_interior_pages_and_without_rowid() {
        use std::io::{BufRead as _, Write as _};

        let dir = aterm_tempfile::tempdir().expect("scratch");
        let db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("live.db");

        // Keep sqlite3 open with autocheckpoint=0 so page 1, interior B-tree pages, overflow
        // chains, and WITHOUT ROWID pages all live inside `-wal`.
        let mut holder = crate::spawnfd::command("/usr/bin/sqlite3")
            .arg(&db)
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::piped())
            .spawn()
            .expect("holder");
        let mut stdin = holder.stdin.take().expect("stdin");

        let big_text = "overflow-segment-".repeat(600); // > 10 KB, spans multiple 4 KB pages
        writeln!(
            stdin,
            "PRAGMA page_size=1024; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; \
             CREATE TABLE goals (id INTEGER PRIMARY KEY, session_id TEXT, objective TEXT, status TEXT); \
             CREATE TABLE kv (k TEXT PRIMARY KEY, v INT) WITHOUT ROWID;"
        )
        .expect("ddl");
        for i in 0..120 {
            writeln!(
                stdin,
                "INSERT INTO goals (session_id, objective, status) VALUES ('s-{i}', 'goal-{i}', 'paused');"
            )
            .expect("insert");
        }
        writeln!(
            stdin,
            "INSERT INTO goals (id, session_id, objective, status) VALUES (999, 'big', '{big_text}', 'active'); \
             INSERT INTO kv VALUES ('alpha', -42), ('beta', 1000000); \
             SELECT 'ready';"
        )
        .expect("big");
        stdin.flush().expect("flush");

        let mut lines = std::io::BufReader::new(holder.stdout.take().expect("stdout")).lines();
        while let Some(Ok(line)) = lines.next() {
            if line == "ready" {
                break;
            }
        }

        let goals = crate::foreign_db::read_bytes(&db, 16 << 20, |main, wal| {
            scan_table(main, wal, "goals", |schema, row| {
                let id_col = schema.column_index("id")?;
                let sid_col = schema.column_index("session_id")?;
                let obj_col = schema.column_index("objective")?;
                let st_col = schema.column_index("status")?;
                if row.get(st_col)?.as_str() == Some("active") {
                    Some((
                        row.get(id_col)?.to_list_field(),
                        row.get(sid_col)?.to_list_field(),
                        row.get(obj_col)?.to_list_field(),
                    ))
                } else {
                    None
                }
            })
        })
        .expect("read_bytes goals");
        assert_eq!(
            goals,
            vec![("999".to_owned(), "big".to_owned(), big_text)],
            "overflow + rowid alias + uncheckpointed WAL"
        );

        let mut kv = crate::foreign_db::read_bytes(&db, 16 << 20, |main, wal| {
            scan_table(main, wal, "kv", |schema, row| {
                Some((
                    row.get(schema.column_index("k")?)?.to_list_field(),
                    row.get(schema.column_index("v")?)?.to_list_field(),
                ))
            })
        })
        .expect("read_bytes kv");
        kv.sort();
        assert_eq!(
            kv,
            vec![
                ("alpha".to_owned(), "-42".to_owned()),
                ("beta".to_owned(), "1000000".to_owned()),
            ],
            "WITHOUT ROWID + signed ints"
        );

        let _ = holder.kill();
        let _ = holder.wait();

        // Corrupt/uncommitted trailing WAL bytes after the last commit frame are ignored.
        let mut wal_path = db.as_os_str().to_os_string();
        wal_path.push("-wal");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&wal_path)
            .expect("open wal");
        f.write_all(&vec![0xAA; 2048]).expect("append torn frame");
        drop(f);

        let count = crate::foreign_db::read_bytes(&db, 16 << 20, |main, wal| {
            scan_table(main, wal, "goals", |_, _| Some(())).map(|v| v.len())
        })
        .expect("torn tail ignored");
        assert_eq!(count, 121);

        // Missing table returns Err(Unread::Failed), matching sqlite3 -bail.
        run_sqlite(&db, "PRAGMA wal_checkpoint(TRUNCATE);");
        assert_eq!(
            crate::foreign_db::read_bytes(&db, 16 << 20, |main, wal| {
                scan_table(main, wal, "no_such_table", |_, _| Some(()))
            }),
            Err(crate::foreign_db::Unread::Failed)
        );

        // Generated columns (`VIRTUAL` default, explicit `VIRTUAL`, and `STORED`) plus
        // single-quoted column identifiers align record decoding with real SQLite storage.
        let gen_db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("generated.db");
        run_sqlite(
            &gen_db,
            "CREATE TABLE gen_test (
                'session_id' TEXT NOT NULL,
                base_val INTEGER NOT NULL,
                virt_implicit INTEGER AS (base_val + 1),
                virt_explicit INTEGER GENERATED ALWAYS AS (base_val + 2) VIRTUAL,
                stored_val INTEGER GENERATED ALWAYS AS (base_val + 3) STORED,
                objective TEXT NOT NULL
            );
            INSERT INTO gen_test ('session_id', base_val, objective)
                VALUES ('sess-1', 10, 'Ship native reader');",
        );

        let rows = crate::foreign_db::read_bytes(&gen_db, 16 << 20, |main, wal| {
            scan_table(main, wal, "gen_test", |schema, row| {
                Some((
                    row.get(schema.column_index("session_id")?)?.to_list_field(),
                    row.get(schema.column_index("base_val")?)?.to_list_field(),
                    row.get(schema.column_index("stored_val")?)?.to_list_field(),
                    row.get(schema.column_index("objective")?)?.to_list_field(),
                ))
            })
        })
        .expect("scan gen_test");
        assert_eq!(
            rows,
            vec![(
                "sess-1".to_owned(),
                "10".to_owned(),
                "13".to_owned(),
                "Ship native reader".to_owned(),
            )]
        );

        // Multi-page `WITHOUT ROWID` with non-prefix composite PRIMARY KEY (exercising both `0x02`
        // interior and `0x0a` leaf index cells) plus `ALTER TABLE ... ADD COLUMN ... DEFAULT`.
        let edge_db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("edge.db");
        let mut sql = String::from(
            "PRAGMA page_size=512;
             CREATE TABLE perm_kv (
                 payload TEXT NOT NULL,
                 \"k1\" TEXT NOT NULL,
                 k2 INTEGER NOT NULL,
                 tag TEXT NOT NULL,
                 PRIMARY KEY (k2 DESC, \"k1\" COLLATE NOCASE)
             ) WITHOUT ROWID;
             CREATE TABLE migrated (
                 id INTEGER PRIMARY KEY,
                 title TEXT NOT NULL
             );
             INSERT INTO migrated (id, title) VALUES (1, 'legacy-row');
             ALTER TABLE migrated ADD COLUMN status TEXT NOT NULL DEFAULT 'active';
             ALTER TABLE migrated ADD COLUMN retries INTEGER NOT NULL DEFAULT (-3);
             ALTER TABLE migrated ADD COLUMN enabled BOOLEAN NOT NULL DEFAULT TRUE;
             INSERT INTO migrated (id, title, status, retries, enabled)
                 VALUES (2, 'modern-row', 'paused', 5, 0);",
        );
        for i in 0..180 {
            sql.push_str(&format!(
                "INSERT INTO perm_kv (payload, \"k1\", k2, tag) VALUES ('val-{i:03}', 'key-{i:03}', {i}, 't-{i}');"
            ));
        }
        run_sqlite(&edge_db, &sql);

        let mut perm_rows = crate::foreign_db::read_bytes(&edge_db, 16 << 20, |main, wal| {
            scan_table(main, wal, "perm_kv", |schema, row| {
                Some((
                    row.get(schema.column_index("k2")?)?
                        .to_list_field()
                        .parse::<usize>()
                        .ok()?,
                    row.get(schema.column_index("payload")?)?.to_list_field(),
                    row.get(schema.column_index("k1")?)?.to_list_field(),
                    row.get(schema.column_index("tag")?)?.to_list_field(),
                ))
            })
        })
        .expect("scan perm_kv");
        perm_rows.sort_by_key(|r| r.0);
        assert_eq!(perm_rows.len(), 180);
        for (idx, (k2, payload, k1, tag)) in perm_rows.into_iter().enumerate() {
            assert_eq!(k2, idx);
            assert_eq!(payload, format!("val-{idx:03}"));
            assert_eq!(k1, format!("key-{idx:03}"));
            assert_eq!(tag, format!("t-{idx}"));
        }

        let migrated_rows = crate::foreign_db::read_bytes(&edge_db, 16 << 20, |main, wal| {
            scan_table(main, wal, "migrated", |schema, row| {
                Some((
                    row.get(schema.column_index("id")?)?.to_list_field(),
                    row.get(schema.column_index("title")?)?.to_list_field(),
                    row.get(schema.column_index("status")?)?.to_list_field(),
                    row.get(schema.column_index("retries")?)?.to_list_field(),
                    row.get(schema.column_index("enabled")?)?.to_list_field(),
                ))
            })
        })
        .expect("scan migrated");
        assert_eq!(
            migrated_rows,
            vec![
                (
                    "1".to_owned(),
                    "legacy-row".to_owned(),
                    "active".to_owned(),
                    "-3".to_owned(),
                    "1".to_owned(),
                ),
                (
                    "2".to_owned(),
                    "modern-row".to_owned(),
                    "paused".to_owned(),
                    "5".to_owned(),
                    "0".to_owned(),
                ),
            ]
        );

        // `scan_table_with` validates required columns even on an empty table, and `INTEGER(8)` /
        // `INTEGER UNSIGNED` PRIMARY KEY columns preserve explicit `NULL` instead of aliasing rowid.
        let quirk_db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("quirk.db");
        run_sqlite(
            &quirk_db,
            "CREATE TABLE empty_goals (session_id TEXT PRIMARY KEY, status TEXT);
             CREATE TABLE sized_pk (id INTEGER(8) PRIMARY KEY, body TEXT);
             CREATE TABLE unsigned_pk (id INTEGER UNSIGNED PRIMARY KEY, body TEXT);
             INSERT INTO sized_pk VALUES (NULL, 'sized-null');
             INSERT INTO unsigned_pk VALUES (NULL, 'unsigned-null');",
        );
        assert_eq!(
            crate::foreign_db::read_bytes(&quirk_db, 16 << 20, |main, wal| {
                scan_table_with(
                    main,
                    wal,
                    "empty_goals",
                    |schema| schema.column_index("objective"),
                    |_, _| Some(()),
                )
            }),
            Err(crate::foreign_db::Unread::Failed),
            "missing required column on an empty table must fail schema preparation"
        );
        for tbl in ["sized_pk", "unsigned_pk"] {
            let rows = crate::foreign_db::read_bytes(&quirk_db, 16 << 20, |main, wal| {
                scan_table(main, wal, tbl, |schema, row| {
                    Some((
                        row.get(schema.column_index("id")?)?.clone(),
                        row.get(schema.column_index("body")?)?.to_list_field(),
                    ))
                })
            })
            .expect("scan non-alias pk");
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].0,
                SqlValue::Null,
                "{tbl} must preserve explicit NULL"
            );
        }

        // UTF-16LE and UTF-16BE databases (both `sqlite_schema.sql` DDL and row text values) plus
        // multi-page `sqlite_schema` (`page_size=512` with 60 tables so page 1 splits to `0x05`).
        for (enc_name, file_name) in [("UTF-16le", "utf16le.db"), ("UTF-16be", "utf16be.db")] {
            let u16_db = std::fs::canonicalize(dir.path())
                .expect("canonical")
                .join(file_name);
            run_sqlite(
                &u16_db,
                &format!(
                    "PRAGMA encoding = '{enc_name}';
                     CREATE TABLE goals (\"session_id\" TEXT PRIMARY KEY, \"objective\" TEXT, status TEXT);
                     INSERT INTO goals VALUES ('s-utf16', 'Ship café 🚀 objective', 'active');"
                ),
            );
            let got = crate::foreign_db::read_bytes(&u16_db, 16 << 20, |main, wal| {
                scan_table_with(
                    main,
                    wal,
                    "goals",
                    |s| Some((s.column_index("session_id")?, s.column_index("objective")?)),
                    |&(sid_i, obj_i), row| {
                        Some((
                            row.get(sid_i)?.to_list_field(),
                            row.get(obj_i)?.to_list_field(),
                        ))
                    },
                )
            })
            .expect("scan utf16 db");
            assert_eq!(
                got,
                vec![("s-utf16".to_owned(), "Ship café 🚀 objective".to_owned())],
                "{enc_name}"
            );
        }

        let many_db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("many_schema.db");
        let mut many_sql = String::from("PRAGMA page_size=512;");
        for t in 0..60 {
            many_sql.push_str(&format!(
                "CREATE TABLE pad_table_{t:02} (col_alpha_{t:02} TEXT, col_beta_{t:02} INTEGER);"
            ));
        }
        many_sql.push_str(
            "CREATE TABLE target_goals (session_id TEXT PRIMARY KEY, objective TEXT, status TEXT);
             INSERT INTO target_goals VALUES ('s-60', 'Deep schema B-tree', 'active');",
        );
        run_sqlite(&many_db, &many_sql);
        let deep = crate::foreign_db::read_bytes(&many_db, 16 << 20, |main, wal| {
            scan_table_with(
                main,
                wal,
                "target_goals",
                |s| s.column_index("objective"),
                |&obj_i, row| Some(row.get(obj_i)?.to_list_field()),
            )
        })
        .expect("scan multi-page sqlite_schema");
        assert_eq!(deep, vec!["Deep schema B-tree".to_owned()]);
    }
}
