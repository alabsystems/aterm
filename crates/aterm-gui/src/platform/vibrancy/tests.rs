// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{MATERIAL_HUD, MATERIAL_SIDEBAR, MATERIAL_UNDER_WINDOW, material_value};
use crate::app_config::BackgroundMaterial;

#[test]
fn material_value_maps_the_exposed_materials() {
    assert_eq!(material_value(BackgroundMaterial::None), None);
    assert_eq!(material_value(BackgroundMaterial::Hud), Some(MATERIAL_HUD));
    assert_eq!(
        material_value(BackgroundMaterial::Sidebar),
        Some(MATERIAL_SIDEBAR)
    );
    assert_eq!(
        material_value(BackgroundMaterial::UnderWindow),
        Some(MATERIAL_UNDER_WINDOW)
    );
    assert_eq!(
        (MATERIAL_SIDEBAR, MATERIAL_HUD, MATERIAL_UNDER_WINDOW),
        (7, 13, 21)
    );
}
