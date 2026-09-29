//! Data-driven elemental affinity -- the third defense layer, and the
//! only one that's a property of the *defender's own elemental nature*
//! rather than their hide or gear. Every creature and race has one (like
//! `natural_defense`'s trait+level), defaulting to `"neutral"` Lvl 1 for
//! anything without a real elemental theme -- 100% damage from
//! everything, no modifier at all.
//!
//! Keyed by *family* (`"water"`, `"fire"`, `"wind"`, `"earth"`, `"holy"`,
//! `"darkness"`, `"energy"`), not by individual `DamageType` -- see
//! `damage::DamageType::element_family`'s own doc for which sub-elements
//! share which family's table today.

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::damage::DamageType;

pub type ElementId = String;

/// Default path for both `server` and `client` when
/// `ARPG_ELEMENT_DEFENSES_PATH` isn't set. Workspace-root-relative,
/// matching how `cargo run` is actually invoked.
pub const DEFAULT_ELEMENT_DEFENSES_PATH: &str = "data/element_defenses.ron";

/// One family's numbers at one level. `primary_counter`/`secondary_counter`
/// reference *other* family ids (not a single `DamageType`) -- e.g.
/// Water's counters are the `"fire"` and `"wind"` families, covering both
/// Wind and Lightning sub-elements at once, matching the source table's
/// combined "Wind/Lightning" column instead of arbitrarily picking one.
///
/// A family with no real secondary relationship (only `"holy"` today --
/// its original "Poison" counter has no home in the new family list, see
/// `element_defense.ron`'s own comment) self-references its own family id
/// as a harmless no-op: `modifier`'s `same_element` check always fires
/// first for a genuine self-hit, so a secondary counter pointing at the
/// same family it's defined on can never actually be reached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementDefenseLevel {
    /// Baseline multiplier for anything that's neither this family, the
    /// primary counter, nor the secondary counter -- covers unrelated
    /// elements *and* plain physical damage.
    pub neutral_physical: f32,
    pub same_element: f32,
    pub primary_counter: ElementId,
    pub primary_multiplier: f32,
    pub secondary_counter: ElementId,
    pub secondary_multiplier: f32,
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct ElementDefenseRegistry {
    /// Index 0 is Level 1, and so on -- same out-of-range clamping as
    /// `natural_defense::NaturalDefenseRegistry`.
    pub families: HashMap<ElementId, Vec<ElementDefenseLevel>>,
}

impl ElementDefenseRegistry {
    /// `1.0` (fully neutral) for a family id or level this registry
    /// doesn't know about -- this is how `"energy"` (no table data given
    /// yet) and `"neutral"` beyond its one defined level both resolve to
    /// "no modifier at all" without needing a special case.
    pub fn modifier(&self, family_id: &str, level: u8, incoming: DamageType) -> f32 {
        let Some(levels) = self.families.get(family_id) else {
            return 1.0;
        };
        let Some(last_index) = levels.len().checked_sub(1) else {
            return 1.0;
        };
        let level = &levels[(level.saturating_sub(1) as usize).min(last_index)];

        let incoming_family = incoming.element_family();
        if incoming_family == Some(family_id) {
            level.same_element
        } else if incoming_family == Some(level.primary_counter.as_str()) {
            level.primary_multiplier
        } else if incoming_family == Some(level.secondary_counter.as_str()) {
            level.secondary_multiplier
        } else {
            level.neutral_physical
        }
    }
}

impl std::str::FromStr for ElementDefenseRegistry {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}
