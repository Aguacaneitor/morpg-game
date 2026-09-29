//! Objects in the map with states a player changes: a rock pile broken
//! open into a hole, a ladder (one state, always open), later a door or a
//! lever. Defined in `data/world_objects.ron`, placed by zone files
//! (`map::ObjectPlacement`), stitched into `map::World::objects`.
//!
//! **Art convention.** An object's art is a folder under
//! `gallery/objects/` (`WorldObjectDefinition::art`) named after its
//! states:
//!
//! - `<state>.png` -- the object in that state, on its own floor.
//! - `below.png` -- a connector seen from the floor below it (the ladder
//!   leaning up to the hatch, the rope under the hole), in every state;
//!   `<state>_below.png` overrides it for one state.
//! - `<from>_to_<to>/0001.png`, `0002.png`, ... -- the frames played
//!   while it changes from one state to the other (`Transition::frames`),
//!   numbered like every other animated object's.
//!
//! An image taller than a tile stands on its cell's bottom edge and
//! reaches up into the one north of it (`client::world_objects`).
//!
//! **Who decides.** The server alone changes an object's state
//! (`server::world_objects`: damage, and going back once nobody's
//! around) and tells every client (`ServerMessage::WorldObjects`). Both
//! sides then play the same transition (`systems::world_objects::
//! tick_world_object_transitions`) and read the same states to move
//! players between floors (`systems::stairs`), so the client predicts
//! going through an open hole exactly as the server does it.

use std::collections::HashMap;

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};

use crate::damage::{DamageType, DamageTypeSpec};
use crate::map::World;
use crate::TICK_RATE_HZ;

pub const DEFAULT_WORLD_OBJECTS_PATH: &str = "data/world_objects.ron";

pub type WorldObjectId = String;
pub type ObjectStateId = String;

/// Reserved `NetworkId` range for placed world objects -- bit 59 set,
/// alongside the same top bit every server-made-up id sets, clear of a
/// chest (bit 62), an NPC (61) or a light orb (60). Deterministic, like a
/// chest's: the `index`-th object in `World::objects`, which client and
/// server stitch from the same zone files in the same order.
pub const WORLD_OBJECT_NETWORK_ID_BASE: u64 = (1u64 << 63) | (1u64 << 59);

pub fn world_object_network_id(index: usize) -> crate::components::NetworkId {
    crate::components::NetworkId(WORLD_OBJECT_NETWORK_ID_BASE + index as u64)
}

/// The `World::objects` index a `world_object_network_id` came from.
pub fn world_object_index(id: crate::components::NetworkId) -> Option<usize> {
    id.0.checked_sub(WORLD_OBJECT_NETWORK_ID_BASE).map(|index| index as usize).filter(|&index| index < 1 << 32)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldObjectDefinition {
    /// Folder under `gallery/objects/` holding its art -- see this
    /// module's doc for what goes in it.
    pub art: String,
    /// The state it's placed in.
    pub initial: ObjectStateId,
    /// Set for an object joining its floor to the one right below it (a
    /// ladder, a hole): it's placed on the upper floor, where the opening
    /// is. From below, interacting next to it always climbs up -- whatever
    /// its state, so nobody is ever trapped down there. From above,
    /// walking onto it goes down only in a state with `down`.
    #[serde(default)]
    pub connector: Option<Connector>,
    pub states: HashMap<ObjectStateId, ObjectStateDefinition>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Connector {
    /// How walking onto the opening from above takes you down.
    pub descent: Descent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Descent {
    /// Climbing down, like a ladder: just the floor change, the player
    /// keeps control.
    Climb,
    /// A real fall: the Falling animation and a moment of
    /// `CombatState::Recovering` (`systems::stairs::tick_fall_through_gaps`).
    Fall,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObjectStateDefinition {
    /// A connector: whether its opening lets you down from above in this
    /// state. Climbing up from below never depends on it.
    #[serde(default)]
    pub down: bool,
    /// What sets it changing to another state.
    #[serde(default)]
    pub trigger: Option<Trigger>,
    /// Going back to another state on its own.
    #[serde(default)]
    pub reset: Option<Reset>,
}

/// What changes an object's state. `Damage` is the only kind so far; one
/// that needs a tool (hit it with a pickaxe) or an item (unlock it with a
/// key) is another variant here, set off by its own server system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Trigger {
    /// Breaks after `hp` damage. Only the share of each hit that's one of
    /// `types` counts: a flail that's 80% blunt deals 80% of its damage to
    /// a blunt-only object, a sword none.
    Damage { hp: f32, types: Vec<DamageType>, then: Transition },
}

impl Trigger {
    pub fn then(&self) -> &Transition {
        match self {
            Trigger::Damage { then, .. } => then,
        }
    }
}

/// Changing to state `to`, playing `frames` frames at `fps` on the way
/// (`<from>_to_<to>/` in the object's art; `0` frames = at once).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub to: ObjectStateId,
    #[serde(default)]
    pub frames: u32,
    #[serde(default = "default_transition_fps")]
    pub fps: f32,
}

fn default_transition_fps() -> f32 {
    10.0
}

impl Transition {
    /// How many simulation ticks it takes -- the object is still in the
    /// old state (and passable only as that one) until they've passed.
    pub fn ticks(&self) -> u32 {
        if self.frames == 0 || self.fps <= 0.0 {
            return 0;
        }
        (self.frames as f64 / self.fps as f64 * TICK_RATE_HZ).ceil() as u32
    }
}

/// Goes back on its own once no player has been within `radius` world
/// units of it -- on its floor, or the one below for a connector -- for
/// `after_secs` seconds (`server::world_objects`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reset {
    pub after_secs: f32,
    pub radius: f32,
    pub then: Transition,
}

impl ObjectStateDefinition {
    /// How much of a hit dealing `damage_type` counts against this state's
    /// `Trigger::Damage` (0 to 1) -- `0` for a state that damage doesn't
    /// change.
    pub fn damage_share(&self, damage_type: &DamageTypeSpec) -> f32 {
        let Some(Trigger::Damage { types, .. }) = &self.trigger else { return 0.0 };
        damage_type.fractions().iter().filter(|(kind, _)| types.contains(kind)).map(|(_, share)| share).sum()
    }

    /// The HP an object starts this state with -- `0` when damage doesn't
    /// change it.
    pub fn starting_hp(&self) -> f32 {
        match &self.trigger {
            Some(Trigger::Damage { hp, .. }) => *hp,
            None => 0.0,
        }
    }
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct WorldObjectRegistry {
    pub objects: HashMap<WorldObjectId, WorldObjectDefinition>,
}

impl std::str::FromStr for WorldObjectRegistry {
    type Err = String;

    /// Parses, then checks that every state it names exists -- a typo'd
    /// state would otherwise only show up as an object stuck mid-way.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let registry: Self = ron::from_str(s).map_err(|e| e.to_string())?;
        for (id, object) in &registry.objects {
            let mut named = vec![&object.initial];
            for state in object.states.values() {
                named.extend(state.trigger.as_ref().map(|trigger| &trigger.then().to));
                named.extend(state.reset.as_ref().map(|reset| &reset.then.to));
            }
            if let Some(missing) = named.into_iter().find(|state| !object.states.contains_key(*state)) {
                return Err(format!("world object '{id}' names state '{missing}', which it doesn't define"));
            }
        }
        Ok(registry)
    }
}

impl WorldObjectRegistry {
    /// The definition of `object`'s `state`, if both exist.
    pub fn state(&self, object: &str, state: &str) -> Option<&ObjectStateDefinition> {
        self.objects.get(object).and_then(|definition| definition.states.get(state))
    }
}

/// One object's state right now. `becoming` is a transition under way --
/// until it finishes, the object still counts as in `state`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldObjectStatus {
    pub state: ObjectStateId,
    pub becoming: Option<Becoming>,
    /// What's left of `state`'s `Trigger::Damage` HP (`0` if it has none).
    pub hp: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Becoming {
    pub to: ObjectStateId,
    pub ticks_left: u32,
    /// The whole transition's length, for drawing how far along it is.
    pub total_ticks: u32,
}

/// Every placed object's `WorldObjectStatus`, indexed like
/// `World::objects`. The server's is the truth; a client's mirrors it from
/// `ServerMessage::WorldObjects`.
#[derive(Debug, Default, Resource)]
pub struct WorldObjectStates {
    pub objects: Vec<WorldObjectStatus>,
}

impl WorldObjectStates {
    /// Every object of `world` in its initial state. Reports placements the
    /// registry can't make sense of -- those just stay inert.
    pub fn new(world: &World, registry: &WorldObjectRegistry) -> Self {
        let objects = world
            .objects
            .iter()
            .map(|placed| {
                let Some(definition) = registry.objects.get(&placed.object) else {
                    eprintln!(
                        "[map] WARNING: object '{}' at (row {}, col {}) on floor {} isn't in data/world_objects.ron -- it does nothing.",
                        placed.object, placed.row, placed.col, placed.level
                    );
                    return WorldObjectStatus { state: String::new(), becoming: None, hp: 0.0 };
                };
                if definition.connector.is_some() && placed.exit.is_none() {
                    eprintln!(
                        "[map] WARNING: connector '{}' at (row {}, col {}) on floor {} has no `exit` -- climbing up lands on its own opening.",
                        placed.object, placed.row, placed.col, placed.level
                    );
                }
                let hp = definition.states.get(&definition.initial).map_or(0.0, ObjectStateDefinition::starting_hp);
                WorldObjectStatus { state: definition.initial.clone(), becoming: None, hp }
            })
            .collect();
        Self { objects }
    }

    /// Starts object `index` changing to `transition.to` -- or switches at
    /// once, for a transition with no frames. Does nothing if it's already
    /// changing.
    pub fn begin(&mut self, index: usize, transition: &Transition, registry: &WorldObjectRegistry, object: &str) {
        let Some(status) = self.objects.get_mut(index) else { return };
        if status.becoming.is_some() {
            return;
        }
        let ticks = transition.ticks();
        if ticks == 0 {
            status.state = transition.to.clone();
            status.hp = registry.state(object, &transition.to).map_or(0.0, ObjectStateDefinition::starting_hp);
        } else {
            status.becoming = Some(Becoming { to: transition.to.clone(), ticks_left: ticks, total_ticks: ticks });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAVE_HOLE: &str = r#"(objects: {
        "hole": (
            art: "terrain/stairs/cave_hole_1",
            initial: "closed",
            connector: Some((descent: Climb)),
            states: {
                "closed": (trigger: Some(Damage(hp: 20.0, types: [Blunt], then: (to: "open", frames: 9, fps: 10.0)))),
                "open": (down: true, reset: Some((after_secs: 300.0, radius: 320.0, then: (to: "closed")))),
            },
        ),
    })"#;

    #[test]
    fn only_the_listed_share_of_a_hit_counts() {
        let registry: WorldObjectRegistry = CAVE_HOLE.parse().unwrap();
        let closed = registry.state("hole", "closed").unwrap();
        assert_eq!(closed.damage_share(&DamageTypeSpec::single(DamageType::Blunt)), 1.0);
        assert_eq!(closed.damage_share(&DamageTypeSpec::single(DamageType::Slashing)), 0.0);
        let flail = crate::damage::DamageMix { Blunt: 0.8, Piercing: 0.2, ..Default::default() };
        assert!((closed.damage_share(&DamageTypeSpec::Mixed(Some(flail))) - 0.8).abs() < 1e-6);
        assert_eq!(registry.state("hole", "open").unwrap().damage_share(&DamageTypeSpec::single(DamageType::Blunt)), 0.0);
    }

    #[test]
    fn a_state_named_but_not_defined_is_refused() {
        let broken = CAVE_HOLE.replace(r#"then: (to: "closed")"#, r#"then: (to: "shut")"#);
        let error = broken.parse::<WorldObjectRegistry>().unwrap_err();
        assert!(error.contains("'shut'"), "{error}");
    }

    #[test]
    fn a_transition_lasts_its_frames_and_an_instant_one_switches_at_once() {
        let registry: WorldObjectRegistry = CAVE_HOLE.parse().unwrap();
        let mut states = WorldObjectStates {
            objects: vec![WorldObjectStatus { state: "closed".into(), becoming: None, hp: 0.0 }; 2],
        };
        let opening = registry.state("hole", "closed").unwrap().trigger.as_ref().unwrap().then();
        assert_eq!(opening.ticks(), 54, "9 frames at 10 fps, at 60 ticks a second");
        states.begin(0, opening, &registry, "hole");
        assert_eq!(states.objects[0].state, "closed", "still closed until the animation ends");
        assert_eq!(states.objects[0].becoming.as_ref().map(|b| b.ticks_left), Some(54));

        let closing = &registry.state("hole", "open").unwrap().reset.as_ref().unwrap().then;
        states.objects[1].state = "open".into();
        states.begin(1, closing, &registry, "hole");
        assert_eq!(states.objects[1].state, "closed", "no frames: at once");
        assert_eq!(states.objects[1].hp, 20.0, "and whole again");
    }

    #[test]
    fn the_shipped_data_file_parses() {
        let registry: WorldObjectRegistry = include_str!("../../data/world_objects.ron").parse().expect("data/world_objects.ron");
        assert!(registry.objects.contains_key("wooden_ladder") && registry.objects.contains_key("cave_hole_1"));
    }

    #[test]
    fn network_ids_round_trip_to_the_placement_index() {
        assert_eq!(world_object_index(world_object_network_id(7)), Some(7));
        assert_eq!(world_object_index(crate::components::NetworkId(5)), None);
    }
}
