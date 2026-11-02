// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lab importer's reading of the accounting sheet's provider cells ([`read_sheet_cell`],
//! `aterm db alab import`, docs/DESIGN-alab-lab-model-2026-11-01.md §9) is the harness's own:
//! for every pooled CLI it agrees with [`parse_accounting_csv_for`] on who is a member, with
//! which region and flow, and who is held. Every fixture email is `example.test`.

use super::*;

/// One row per address, each with a machine, so no row conflicts and every member row is one
/// the harness takes.
const SHEET: &str = "email,computer,claude,Open AI,google,meta,kimi,x,notes\n\
a1@example.test,m27,1,browser,1,,global,device,\n\
a2@example.test,m28,1,1,,1,1,,kimi banned\n\
a3@example.test,m29,0,pro,1,,cn,1,banned on google\n\
a4@example.test,m30,1,device,,,kimi.com,browser,claude suspended\n\
a5@example.test,m31,,,,1,1,,\n\
a6@example.test,m32,1,,1,1,,,meta deactivated\n\
a7@example.test,m33,phone,0,,,,,payment disabled\n";

const COLUMNS: [(SheetColumn, usize); 6] = [
    (SheetColumn::Pool(Cli::Claude), 2),
    (SheetColumn::Pool(Cli::Codex), 3),
    (SheetColumn::Google, 4),
    (SheetColumn::Meta, 5),
    (SheetColumn::Pool(Cli::Kimi), 6),
    (SheetColumn::Pool(Cli::Grok), 7),
];

fn rows() -> Vec<Vec<String>> {
    SHEET.lines().skip(1).map(parse_csv_line).collect()
}

fn vendor(region: Option<Region>, flow: Option<Flow>) -> accounts::Vendor {
    accounts::Vendor { region, flow }
}

/// Each provider cell reads as the harness reads the sheet: Kimi Code's bare `1` is held and its
/// vendor words name a region, Codex's and Grok Build's name a flow, `claude` takes only `1`, a
/// value no column takes is unread, and a disqualifying note drops Claude Code whatever it names
/// but any other vendor only when it names that one or none. For every pooled CLI the members,
/// their vendor keys and the held rows agree with [`parse_accounting_csv_for`] on the same sheet.
/// NEGATIVE CONTROL: with `read_sheet_cell` reading Kimi Code's held cell as a member with no
/// region (`Member::Held => SheetCell::Member(accounts::Vendor::default())`), a5's Kimi cell reads
/// as a member and the explicit table fails: `` left: Member(Vendor { region: None, flow: None })
/// right: Held ``.
#[test]
fn lab_cells_read_as_the_harness_reads_the_sheet() {
    use SheetCell::{Absent, Disqualified, Held, Member, Unread};
    let plain = vendor(None, None);
    let expected = [
        // claude, codex, google, meta, kimi, grok
        [
            Member(plain),
            Member(vendor(None, Some(Flow::Browser))),
            Member(plain),
            Absent,
            Member(vendor(Some(Region::Global), None)),
            Member(vendor(None, Some(Flow::Device))),
        ],
        [
            Disqualified(plain),
            Member(plain),
            Absent,
            Member(plain),
            Disqualified(plain),
            Absent,
        ],
        [
            Absent,
            Unread,
            Disqualified(plain),
            Absent,
            Member(vendor(Some(Region::MainlandCn), None)),
            Member(plain),
        ],
        [
            Disqualified(plain),
            Member(vendor(None, Some(Flow::Device))),
            Absent,
            Absent,
            Member(vendor(Some(Region::MainlandCn), None)),
            Member(vendor(None, Some(Flow::Browser))),
        ],
        [Absent, Absent, Absent, Member(plain), Held, Absent],
        [
            Disqualified(plain),
            Absent,
            Member(plain),
            Disqualified(plain),
            Absent,
            Absent,
        ],
        [Unread, Absent, Absent, Absent, Absent, Absent],
    ];
    for (row, want) in rows().iter().zip(expected) {
        for ((column, at), want) in COLUMNS.iter().zip(want) {
            assert_eq!(
                read_sheet_cell(*column, &row[*at], &row[8]),
                want,
                "{} {column:?}",
                row[0]
            );
        }
    }
    for (column, at) in COLUMNS {
        let SheetColumn::Pool(cli) = column else {
            continue;
        };
        let fleet = parse_accounting_csv_for(cli, SHEET, "lab-cells.csv").unwrap();
        for row in rows() {
            let member = fleet
                .roster
                .accounts
                .iter()
                .find(|account| account.email == row[0])
                .map(|account| account.vendor);
            let held = fleet.held.iter().any(|account| account.email == row[0]);
            let read = read_sheet_cell(column, &row[at], &row[8]);
            match read {
                Member(vendor) => assert_eq!(member, Some(vendor), "{} {cli:?}", row[0]),
                Held => assert!(held && member.is_none(), "{} {cli:?}", row[0]),
                Absent | Unread | Disqualified(_) => {
                    assert!(!held && member.is_none(), "{} {cli:?}", row[0]);
                }
            }
        }
    }
}
