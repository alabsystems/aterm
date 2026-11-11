// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The default nickname: a short noun drawn once from the OS's CSPRNG when a machine's
//! name file is first made (`docs/DESIGN-machine-names-2026-11-04.md` §2.4).
//!
//! Each noun is 3 to 7 lowercase ASCII letters, a singular concrete noun (animals,
//! birds, fish, trees, plants, stones, weather, landscape, simple objects), at least two
//! edits from every other and a prefix of none. Left out: given names, brands and
//! products (software projects, tools, libraries, commands, other companies' computer
//! models, OS release names; the animal half of an Ubuntu codename is kept, since a
//! release is called by its adjective), places, Apple model, chip and core-codename
//! words, aterm, shell and computing words, number and letter homophones, hex-only words,
//! words that insult the machine, and anything with a slang, crude, sexual, violent,
//! medical, drug, religious, ethnic or political sense, homophones and common misreadings
//! included. `nouns_are_well_formed` checks what the words themselves show; their senses
//! are a judgement, so every word added gets an agent's hostile review against those
//! senses before it lands, and a word any review flags is dropped, not argued.

/// The most random draws before a bounded search of the remaining noun pool.
pub const DRAWS: usize = 8;

/// The default nicknames: every word that passed review, so their number is whatever
/// that leaves (far above the fleet's machines, which is all the draw needs).
pub const NOUNS: &[&str] = &[
    "heron", "otter", "puffin", "curlew", "egret", "ibis", "stork", "crane", "finch", "lark",
    "sparrow", "warbler", "bunting", "oriole", "magpie", "crow", "jackdaw", "owl", "eagle", "hawk",
    "falcon", "osprey", "harrier", "condor", "tern", "gull", "pelican", "pigeon", "quail",
    "grouse", "emu", "ostrich", "grebe", "hoopoe", "parrot", "macaw", "budgie", "weaver",
    "waxwing", "shrike", "lapwing", "goshawk", "cygnet", "swan", "goose", "mallard", "eider",
    "badger", "panda", "koala", "walrus", "marmot", "stoat", "mink", "sable", "ermine", "hare",
    "vole", "raccoon", "possum", "wombat", "quokka", "dingo", "jackal", "wolf", "ocelot", "zebra",
    "giraffe", "hippo", "tapir", "okapi", "ibex", "pony", "foal", "sheep", "goat", "manatee",
    "dolphin", "narwhal", "beluga", "orca", "whale", "deer", "caribou", "fawn", "mammoth",
    "hamster", "salmon", "trout", "cod", "haddock", "halibut", "tuna", "marlin", "herring",
    "sardine", "anchovy", "minnow", "koi", "grouper", "shark", "eel", "urchin", "mussel", "oyster",
    "scallop", "conch", "abalone", "krill", "shrimp", "lobster", "anemone", "kelp", "ladybug",
    "cricket", "cicada", "mantis", "moth", "mayfly", "hornet", "scarab", "spider", "toad",
    "iguana", "lizard", "turtle", "axolotl", "oak", "birch", "alder", "maple", "cedar", "pine",
    "spruce", "fir", "larch", "linden", "poplar", "walnut", "hickory", "pecan", "almond", "baobab",
    "banyan", "acacia", "dogwood", "redwood", "balsa", "ginkgo", "plum", "apricot", "mango",
    "papaya", "lychee", "berry", "gourd", "pumpkin", "moss", "lichen", "thistle", "clover",
    "tulip", "orchid", "aster", "crocus", "zinnia", "freesia", "lilac", "parsley", "fennel",
    "chive", "garlic", "kale", "cress", "bramble", "barley", "wheat", "maize", "rice", "cactus",
    "agave", "aloe", "yucca", "begonia", "azalea", "granite", "basalt", "marble", "slate", "topaz",
    "pumice", "sand", "gravel", "citrine", "lapis", "iron", "copper", "zinc", "cobalt", "nickel",
    "silver", "gold", "bronze", "pewter", "geode", "sleet", "frost", "dew", "mist", "breeze",
    "gust", "comet", "star", "planet", "nebula", "quasar", "pulsar", "sunrise", "creek", "lake",
    "lagoon", "inlet", "fjord", "canyon", "gorge", "valley", "prairie", "desert", "ridge", "crag",
    "peak", "summit", "hill", "glacier", "island", "isle", "atoll", "shore", "coast", "marsh",
    "grove", "thicket", "jungle", "plateau", "crater", "volcano", "geyser", "estuary", "cave",
    "grotto", "lantern", "candle", "kettle", "teapot", "ladle", "spoon", "cup", "vase", "basket",
    "barrel", "chair", "quilt", "blanket", "thimble", "ribbon", "kite", "sled", "canoe", "paddle",
    "sail", "compass", "hammock", "tent", "cabin", "hut", "barn", "ladder", "wagon", "cart",
    "drum", "flute", "banjo", "cello", "oboe", "bugle", "gong", "wok", "skillet", "sieve",
    "bottle", "pitcher", "goblet", "saucer", "apron", "scarf", "sock", "cloak", "vest", "jacket",
    "comb", "abacus", "globe", "easel", "crayon", "eraser", "puzzle", "hearth", "chimney", "attic",
    "porch", "hammer", "chisel", "lever", "pulley", "wedge", "piston", "discus", "sunfish",
    "nutmeg", "loch", "cairn", "basin", "lava", "magma", "cinder", "trowel", "shovel", "anvil",
    "mallet", "gazebo", "teacup", "platter", "kiln", "satchel", "parasol", "beacon", "rudder",
    "dinghy", "barge", "ketch", "gondola", "trolley", "marimba", "ocarina", "puppy", "hen",
    "rooster", "ferret", "echidna", "manta", "coral", "collie", "terrier", "corgi", "spaniel",
    "tub", "piano", "violin", "guitar", "cymbal", "rattle", "bagpipe", "balloon", "cushion",
    "curtain", "carpet", "mop", "ravine", "spire", "stone", "terrace", "orchard", "garden",
    "pasture", "glade", "orbit", "sunbeam", "icicle", "tadpole", "petunia", "bonsai", "peanut",
    "cashew", "lime", "grape", "raisin", "tomato", "turnip", "radish", "parsnip", "lentil",
    "spinach", "celery", "lettuce", "okra", "skylark", "canal", "wharf", "whisk", "locket",
    "brooch", "pendant", "tiara", "feather", "sundial", "bicycle", "scooter", "sleigh", "tractor",
    "chariot", "caravan", "yacht", "glider", "igloo", "pipit", "linnet", "siskin", "junco",
    "tanager", "skua", "avocet", "dunlin", "moorhen", "bittern", "eland", "dugong", "tarpon",
    "bonito", "bream", "tench", "sprat", "turbot", "whelk", "caddis", "skink", "catalpa", "kauri",
    "bracken", "cowslip", "sorrel", "sedum", "yarrow", "sundew", "spinel", "peridot", "calcite",
    "shoal", "pestle", "fulmar", "murre", "jacana", "motmot", "potoo", "trogon", "tinamou",
    "limpkin", "bulbul", "takahe", "agouti", "coati", "tarsier", "awning", "carafe", "cenote",
    "chalet", "cotinga", "dowel", "moraine", "pergola", "pochard", "sloop", "verbena", "zither",
    "broom", "numbat",
];

/// A default nickname: up to [`DRAWS`] nouns drawn with `next` (uniform `u32`s from the
/// OS's CSPRNG), the first one `in_use` says no machine in view uses as a nickname or
/// nickname alias (compared without case). After occupied random draws, walk the pool
/// from the last draw so an available noun is never missed. `None` means every noun is in use.
///
/// The index is `next() % NOUNS.len()`: with a few hundred nouns its bias is below 2^-22,
/// which no person can see.
#[must_use]
pub fn draw(mut next: impl FnMut() -> u32, in_use: impl Fn(&str) -> bool) -> Option<&'static str> {
    let mut last = 0;
    for _ in 0..DRAWS {
        last = next() as usize % NOUNS.len();
        let noun = NOUNS[last];
        if !in_use(noun) {
            return Some(noun);
        }
    }
    (1..NOUNS.len())
        .map(|offset| NOUNS[(last + offset) % NOUNS.len()])
        .find(|noun| !in_use(noun))
}
