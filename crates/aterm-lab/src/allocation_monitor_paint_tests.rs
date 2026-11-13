// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn audit(first_y: f64, second_y: f64, second_text: bool) -> String {
    format!(
        "inspection-source scale=1 viewport=0,0,100,100\npaint-node key=\"allocation/content\" rect=0,0,100,100 clip=0,0,100,100 visible=true clipped=false\npaint-node key=\"allocation/monitor/source\" rect=0,{first_y},100,21 clip=0,0,100,100 visible=true clipped=false label=\"source\"\npaint-text key=\"allocation/monitor/source\" overflow=false clip-truncated=false painted=\"source\"\npaint-node key=\"allocation/monitor/footer\" rect=0,{second_y},100,21 clip=0,0,100,100 visible=true clipped=false label=\"footer\"\n{}",
        if second_text {
            "paint-text key=\"allocation/monitor/footer\" overflow=false clip-truncated=false painted=\"footer\"\n"
        } else {
            ""
        }
    )
}

#[test]
fn partial_scroll_edges_do_not_count_but_overlapping_frames_cover_the_whole_row() {
    let semantic = BTreeMap::from([
        ("allocation/monitor/source".into(), "source".into()),
        ("allocation/monitor/footer".into(), "footer".into()),
    ]);
    let (first, _) = witness(&audit(10.0, 83.0, false), &semantic).unwrap();
    assert_eq!(first, BTreeSet::from(["allocation/monitor/source".into()]));
    let (second, _) = witness(&audit(-10.0, 63.0, true), &semantic).unwrap();
    assert_eq!(second, BTreeSet::from(["allocation/monitor/footer".into()]));
    assert!(
        semantic
            .keys()
            .all(|key| first.union(&second).any(|covered| covered == key))
    );
    assert!(
        witness(&audit(10.0, 63.0, false), &semantic).is_none(),
        "a wholly visible node without paint cannot qualify"
    );
    assert!(
        witness(
            &audit(10.0, 63.0, true).replace("painted=\"footer\"", "painted=\"different cost\""),
            &semantic
        )
        .is_none()
    );
    assert!(
        witness(
            &audit(10.0, 63.0, true).replace("overflow=false", "overflow=true"),
            &semantic
        )
        .is_none()
    );
    assert!(
        witness(
            &audit(10.0, 63.0, true).replace("clip-truncated=false", "clip-truncated=true"),
            &semantic
        )
        .is_none()
    );
}
