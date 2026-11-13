// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn pets_resolve_trimmed_and_case_insensitively_and_off_draws_none() {
    assert_eq!(PET_NAMES, ["off", "cat", "dog", "flying cat"]);
    assert_eq!(species(DEFAULT_PET), Some(PetSpecies::Cat));
    assert_eq!(species("\u{00a0}DOG\u{2003}"), Some(PetSpecies::Dog));
    assert_eq!(canonical(" Off "), Some("off"));
    assert_eq!(species("off"), None);
    // A flier keeps no walker, and only a flier flies.
    assert_eq!((species(FLYING_CAT), flies(" Flying Cat ")), (None, true));
    for pet in PETS {
        assert_eq!(pet.flies, pet.name == FLYING_CAT, "{}", pet.name);
        assert!(!(pet.flies && pet.species.is_some()), "{}", pet.name);
    }
    for unknown in ["", "kitty", "fox", "cat pet", "rainbow dog pet"] {
        assert_eq!(row(unknown), None, "{unknown:?}");
    }
    for pet in PETS {
        assert_eq!(pet.name, pet.name.trim().to_ascii_lowercase());
        assert_eq!(canonical(pet.label), Some(pet.name));
        assert!(!pet.blurb.is_empty());
    }
}
