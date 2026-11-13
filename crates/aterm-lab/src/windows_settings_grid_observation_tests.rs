// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn tabs() -> String {
    use aterm_types::app_inspection::{InspectionEnvelope, InspectionSubject};
    format!(
        "{}\nwindow id=0 rows=24 cols=80\n",
        InspectionEnvelope {
            revision: 3,
            subject: InspectionSubject::Tabs,
        }
        .header_line()
    )
}

const AUDIT: &str = "inspection-source source=presented view=3 window=0 generation=1 geometry=9536910db77ae1d7 compiled-fingerprint=0123456789abcdef capture-serial=8 presentation-revision=9 model-current=true\n";

#[test]
fn qualification_grid_and_presented_identity_are_observed_strictly() {
    let reading = grid_observation(&tabs(), AUDIT).unwrap();
    assert_eq!((reading.rows, reading.cols), (24, 80));
    assert!(reading.presented && reading.current);
    assert_eq!(reading.capture, 8);
    for bad in [
        tabs().replace("cols=80", ""),
        tabs().replace("rows=24", "rows=0"),
        tabs().replace("rows=24", "rows=24 rows=25"),
        format!("{}window id=1 rows=24 cols=80\n", tabs()),
        "ERR unavailable".into(),
    ] {
        assert!(grid_observation(&bad, AUDIT).is_err());
    }
    for bad in [
        AUDIT.replace("geometry=9536910db77ae1d7", "geometry=invalid"),
        AUDIT.replace("capture-serial=8", ""),
        AUDIT.replace("view=3", ""),
        AUDIT.replace("window=0", "window=1"),
        AUDIT.replace("generation=1", ""),
        AUDIT.replace("compiled-fingerprint=0123456789abcdef", ""),
        AUDIT.replace("capture-serial=8", "capture-serial=8 capture-serial=9"),
        format!("{AUDIT}{AUDIT}"),
    ] {
        assert!(grid_observation(&tabs(), &bad).is_err());
    }
    let staged =
        grid_observation(&tabs(), &AUDIT.replace("source=presented", "source=staged")).unwrap();
    assert!(!staged.presented);
    let stale = grid_observation(
        &tabs(),
        &AUDIT.replace("model-current=true", "model-current=false"),
    )
    .unwrap();
    assert!(!stale.current);
}

#[test]
fn explicit_grid_cases_are_ceiling_projections_not_pixel_acceptance() {
    assert_eq!(projected_grid(318, 516), Ok((22, 33)));
    assert_eq!(projected_grid(584, 348), Ok((15, 63)));
    assert_eq!(projected_grid(1095, 696), Ok((30, 119)));
    assert!(projected_grid(24, 516).is_err());
    assert!(projected_grid(318, 12).is_err());
}
