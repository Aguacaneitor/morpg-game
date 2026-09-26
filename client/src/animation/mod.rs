//! Client-only: turns simulation facts `game_core` already tracks
//! (`Facing`, `CombatState`) into which PixelLab-exported texture to
//! show. Nothing here mutates gameplay state -- it only reads it and
//! swaps a `Handle<Image>`, the same "render is a passenger" rule as
//! `sync_sprite_transforms` in main.rs.
//!
//! Two parallel, independently-gated systems live here:
//! `animate_players` (one hardcoded sprite set, every player looks the
//! same regardless of race -- a pre-existing simplification, not
//! something creatures needed to fix) and `animate_creatures` (one
//! sprite set *per* `CreatureId`, loaded straight from whatever
//! `CreatureRegistry` has -- adding a second creature is adding a
//! `data/creatures.ron` entry and a `gallery/animals/<id>` folder, no
//! code change here). Both own death the same way: once `CombatState::
//! Dead`, play the Dying animation exactly once (no looping), then hold
//! on a dedicated static corpse image forever -- nothing here ever
//! despawns a dead creature or a player's corpse (`server::loot::
//! spawn_player_corpses`), that's left for whatever "the body gets used/
//! eaten/looted" mechanic comes later. `animate_players` additionally
//! owns `CombatState::Recovering` (just fell through a floor gap,
//! `game_core::components::FallRecoveryTimer`): plays the Falling
//! animation once, then holds on its own last frame for whatever's left
//! of the lockout.

mod loading;
mod players;
mod creatures;
mod npcs;
mod objects;

use std::collections::HashMap;

use bevy::prelude::*;
use rand::Rng;

use game_core::creature::CreatureId;
use game_core::npc::NpcId;

use creatures::animate_creatures;
use loading::{load_creature_sprites, load_npc_sprites, load_player_sprites};
use npcs::animate_npcs;
use objects::animate_objects;
use players::{animate_players, sync_local_weapon_type};

const RUN_FPS: f32 = 10.0;

const IDLE_FPS: f32 = 6.0;

const JUMP_FPS: f32 = 10.0;

/// Matches `GameplayConfig::attack_duration_ticks` (30 ticks @ 60hz =
/// 0.5s) against the 6 frames `gallery/characters/<race>/animations/
/// Attacking` actually has -- 6 / 0.5 = 12, one clean cycle per swing.
/// If either number changes, retune this to match.
const ATTACK_FPS: f32 = 12.0;

/// Unlike `ATTACK_FPS` (tuned to one exact player attack's frame count
/// against `GameplayConfig::attack_duration_ticks`), a creature's own
/// commit length varies per `creature::CreatureAttack::kind` (a `Slam`'s
/// several snapshots plus recovery run well past just
/// `duration_ticks`) -- this just cycles the `Attacking` art at a
/// reasonable, readable rate for however long `CombatState::Attacking`
/// actually holds, the same "loop for as long as the state lasts" story
/// `Running`/`Idle` already use, rather than chasing an exact per-attack
/// sync.
const CREATURE_ATTACK_FPS: f32 = 12.0;

/// `gallery/animals/<id>/animations/Dying` and `gallery/characters/
/// <race>/animations/Dying` both have 9 frames -- picked so the animation
/// takes a little under a second, long enough to actually read as a death
/// rather than a flinch.
const DYING_FPS: f32 = 10.0;

/// `gallery/characters/<race>/animations/Falling` has 7 frames -- same
/// "reads clearly, doesn't drag" reasoning as `DYING_FPS`. Deliberately
/// not tied to `GameplayConfig::fall_recovery_ticks`/`FallRecoveryTimer`
/// (unlike `ATTACK_FPS`'s exact tick-count match): the animation plays
/// through once and then holds on its last frame for however much of the
/// recovery lockout remains (see `animate_players`), so a shorter/longer
/// recovery (a future race/skill bonus) never needs this retuned to match.
const FALLING_FPS: f32 = 10.0;

/// A little slower than `RUN_FPS` -- shoving something heavy reads as
/// more effortful than an ordinary jog.
const PUSHING_FPS: f32 = 8.0;

/// Which loaded animation is currently playing. `Jumping` is only ever
/// produced for players -- no creature jumps. `Attacking` (creatures) or
/// one of the weapon-specific `Attacking*` variants/`Casting`
/// (players -- see `animate_players`' own doc for how those five get
/// picked) is produced for `CombatState::Attacking`. `Dying` is produced
/// by both too now -- `animate_players` plays it for `CombatState::Dead`
/// the same "play once, then hold on a static `death` image" way
/// `animate_creatures` already does. `Falling`/`Pushing` are player-only
/// (no creature ever falls through a floor gap, or gets a distinct
/// pushing pose, today), played for `CombatState::Recovering`/a live
/// `components::Pushing` respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimKind {
    Idle,
    Running,
    Jumping,
    Attacking,
    /// Player-only -- see `animate_players`' own doc.
    AttackingSword,
    AttackingBow,
    AttackingSpear,
    /// Player-only, an ability being cast (charging or the release
    /// itself) -- see `animate_players`' own doc for exactly what drives
    /// this.
    Casting,
    /// Player-only -- shown instead of `Running` while `components::
    /// Pushing` is true.
    Pushing,
    Dying,
    Falling,
}

impl AnimKind {
    /// True for every kind `animate_players`' own attack-priority block
    /// produces -- see that function's own doc for why grouping these
    /// matters: once one of them has genuinely started playing, it keeps
    /// going (even past the live `CombatState`/`AimIndicator` signal that
    /// started it) until it reaches its own last frame, rather than
    /// cutting off mid-clip.
    fn is_attack_family(self) -> bool {
        matches!(
            self,
            AnimKind::Attacking
                | AnimKind::AttackingSword
                | AnimKind::AttackingBow
                | AnimKind::AttackingSpear
                | AnimKind::Casting
        )
    }
}

/// Folder names PixelLab exports, in `Facing`'s own declaration order --
/// lets client code index with `facing as usize` instead of a match.
const DIRECTION_FOLDERS: [&str; 8] = [
    "south",
    "south-east",
    "east",
    "north-east",
    "north",
    "north-west",
    "west",
    "south-west",
];

/// One `Vec` of frames per direction -- a `Vec`, not a fixed-size array,
/// because different animations genuinely have different frame counts
/// (Idle/Running/Jumping/Attacking/Dying all differ), and now different
/// characters can too -- see `load_direction_frames`, which sources the
/// actual count from each character's own `metadata.json`.
type DirectionFrames = [Vec<Handle<Image>>; 8];

/// All textures for the one hardcoded player character (`characters/human`
/// -- every player looks the same regardless of `CharacterRace` today, a
/// pre-existing simplification `load_player_sprites`'s own doc already
/// flagged; per-race sprite sets, one `PlayerSprites`-like set per
/// `RaceId` mirroring how `CreatureSprites` already works per `CreatureId`,
/// is the natural follow-up once `dwarf`/`elf`/`orc` have their own
/// `Dying`/`Falling`/`death` art to go with the `Idle`/`Running`/
/// `Jumping`/`Attacking` they already have), preloaded once at startup.
#[derive(Resource)]
pub struct PlayerSprites {
    idle: DirectionFrames,
    running: DirectionFrames,
    jumping: DirectionFrames,
    /// The generic swing -- used for unarmed, or a weapon whose own
    /// `item::ItemDefinition::weapon_type` has no dedicated clip below
    /// (`"axe"`/`"mace"`/`"staff"`/`"crossbow"` today) or isn't set at
    /// all. Also what `attacking_sword`/`attacking_bow`/`attacking_spear`
    /// themselves fall back to for a character with no dedicated art of
    /// their own for that weapon type -- see `load_player_sprites`'s own
    /// `["Attacking_*", "Attacking"]` load order.
    attacking: DirectionFrames,
    /// One clip per weapon type this project currently has dedicated art
    /// for -- see `animate_players`' own doc for how the equipped
    /// weapon's `weapon_type` picks between these and the generic
    /// `attacking` above.
    attacking_sword: DirectionFrames,
    attacking_bow: DirectionFrames,
    attacking_spear: DirectionFrames,
    /// An ability being cast -- see `AnimKind::Casting`'s own doc. Falls
    /// back to `attacking` (not any weapon-specific clip -- a spell has
    /// no weapon backing it) for a character with no `Casting` art.
    casting: DirectionFrames,
    dying: DirectionFrames,
    falling: DirectionFrames,
    /// Shown instead of `running` while `components::Pushing` is true --
    /// falls back to `running` for a character with no dedicated art.
    pushing: DirectionFrames,
    /// One static per-direction image (`characters/human/death/`), shown
    /// once `dying` has finished playing through -- same "corpse stays on
    /// the ground" resting state `CreatureAnimSet::death` already has.
    death: [Handle<Image>; 8],
    sounds: AnimSounds,
}

impl PlayerSprites {
    /// The frame set for one of `AnimKind::is_attack_family`'s five
    /// members -- factored out since `animate_players` needs to resolve
    /// this from two different places (a fresh choice, and "whatever was
    /// already playing, continued") that must always agree. Panics for
    /// any other `AnimKind` -- every caller already only ever passes one
    /// of the five, so this is a logic-error guard, not a real runtime
    /// case.
    fn attack_frames(&self, kind: AnimKind) -> &DirectionFrames {
        match kind {
            AnimKind::Attacking => &self.attacking,
            AnimKind::AttackingSword => &self.attacking_sword,
            AnimKind::AttackingBow => &self.attacking_bow,
            AnimKind::AttackingSpear => &self.attacking_spear,
            AnimKind::Casting => &self.casting,
            _ => unreachable!("attack_frames called with a non-attack-family AnimKind"),
        }
    }
}

/// This animation's sound cue *variants* (see `load_animation_sounds`),
/// resolved once at load time the same way its matching `DirectionFrames`
/// set is. Almost always 0 or 1 entries; more than one means
/// `play_anim_sound` picks a random one each time this animation starts,
/// so e.g. a whole field of sheep don't all bleat in exact unison. Kept
/// as named fields rather than a `HashMap<AnimKind, _>` since only these
/// 4 kinds have ever needed a cue authored for them; `Jumping`/`Falling`
/// simply have none (`get` returns `&[]`, silent) rather than empty
/// fields sitting unused here.
#[derive(Default)]
struct AnimSounds {
    idle: Vec<Handle<AudioSource>>,
    running: Vec<Handle<AudioSource>>,
    attacking: Vec<Handle<AudioSource>>,
    dying: Vec<Handle<AudioSource>>,
    casting: Vec<Handle<AudioSource>>,
    pushing: Vec<Handle<AudioSource>>,
}

impl AnimSounds {
    fn get(&self, kind: AnimKind) -> &[Handle<AudioSource>] {
        match kind {
            AnimKind::Idle => &self.idle,
            AnimKind::Running => &self.running,
            // Every weapon-specific swing reuses the one generic swing
            // cue for now -- no per-weapon sound has been authored yet;
            // splitting this out is a data-only follow-up (see
            // `load_player_sprites`) whenever one is.
            AnimKind::Attacking | AnimKind::AttackingSword | AnimKind::AttackingBow | AnimKind::AttackingSpear => {
                &self.attacking
            }
            AnimKind::Casting => &self.casting,
            AnimKind::Pushing => &self.pushing,
            AnimKind::Dying => &self.dying,
            AnimKind::Jumping | AnimKind::Falling => &[],
        }
    }
}

struct CreatureAnimSet {
    idle: DirectionFrames,
    running: DirectionFrames,
    attacking: DirectionFrames,
    dying: DirectionFrames,
    /// One static per-direction image (`gallery/animals/<id>/death/`),
    /// shown once `dying` has finished playing through -- the "corpse
    /// stays on the ground" state, not part of the frame cycle.
    death: [Handle<Image>; 8],
    sounds: AnimSounds,
}

/// One `CreatureAnimSet` per entry in `CreatureRegistry`, preloaded once
/// at startup the same way `PlayerSprites` is.
#[derive(Resource)]
pub struct CreatureSprites {
    sets: HashMap<CreatureId, CreatureAnimSet>,
}

/// Idle + Running only -- an `Npc` has no attack, no death, nothing else
/// to animate at all (see `game_core::npc`'s own module doc). Much
/// smaller than `CreatureAnimSet` on purpose, not a partial version of it.
struct NpcAnimSet {
    idle: DirectionFrames,
    running: DirectionFrames,
}

/// One `NpcAnimSet` per entry in `NpcRegistry`, preloaded once at
/// startup the same way `CreatureSprites`/`PlayerSprites` are.
#[derive(Resource)]
pub struct NpcSprites {
    sets: HashMap<NpcId, NpcAnimSet>,
}

/// Per-entity animation playback position. Resets whenever the active
/// `AnimKind` changes so switching Idle<->Moving<->Jumping never carries
/// over a frame index from a different animation's cycle. Shared by both
/// `animate_players` and `animate_creatures` -- exactly one of the two
/// ever touches a given entity (gated by `Player`/`Creature`), so there's
/// no risk of them fighting over it.
#[derive(Component, Default)]
pub struct AnimationState {
    frame: usize,
    elapsed: f32,
    last_kind: Option<AnimKind>,
}

impl AnimationState {
    /// A starting state for an entity (player *or* creature -- both
    /// `animate_players`/`animate_creatures` honor this the same way)
    /// that's already dead the moment it's spawned -- see `net::
    /// apply_remote_snapshots`'s own doc for why this has to exist: an
    /// entity that died while out of the local player's vision (or a
    /// player corpse, `server::loot::spawn_player_corpses`, first seen
    /// long after the moment of death) gets despawned/never predicted
    /// locally, then a *brand new* entity (with a brand new, default
    /// `AnimationState`) once it's actually seen. `AnimationState::
    /// default()`'s `last_kind: None` reads as "just started dying this
    /// frame", replaying the entire death animation on what should
    /// already be a motionless corpse. `frame` is set past any real
    /// animation's length so the "already played through once" check
    /// both `animate_players`/`animate_creatures` do trips immediately,
    /// skipping straight to the resting corpse image instead of frame 0.
    pub fn already_dead() -> Self {
        Self { frame: usize::MAX, elapsed: 0.0, last_kind: Some(AnimKind::Dying) }
    }
}

/// This player's own equipped weapon's `game_core::item::
/// ItemDefinition::weapon_type` (`"sword"`, `"bow"`, `"spear"`, ...) --
/// mirrored the same "local predicts directly off its own real
/// `Equipment`, remote reads the snapshot" split every other charge-
/// adjacent indicator in this project already uses (see
/// `sync_local_weapon_type`'s own doc): a remote entity has no
/// `Equipment` component of its own to read locally at all (only the
/// local player ever gets one -- see `client::net::apply_remote_
/// snapshots`' own spawn site), so it reads `protocol::EntitySnapshot::
/// weapon_type` instead. `None` for unarmed, or a weapon whose own
/// `weapon_type` isn't set. Read by `animate_players` to pick which
/// weapon-specific `Attacking` clip applies -- an unrecognized string
/// (a weapon type with no dedicated art) falls back to the plain
/// `Attacking` clip exactly the same way `None` does.
#[derive(Component, Default)]
pub struct WeaponTypeIndicator(pub Option<String>);

/// A looping animation for a map object (a bonfire, ...) -- unlike
/// `AnimationState`, there's no direction or Idle/Moving state to react
/// to, just "cycle through these frames forever" -- see `client::map`,
/// which is what actually spawns one of these per `object_name` tile.
#[derive(Component)]
pub struct ObjectAnimation {
    frames: Vec<Handle<Image>>,
    fps: f32,
    frame: usize,
    elapsed: f32,
}

impl ObjectAnimation {
    pub fn new(frames: Vec<Handle<Image>>, fps: f32) -> Self {
        Self { frames, fps, frame: 0, elapsed: 0.0 }
    }
}

pub struct AnimationPlugin;

impl Plugin for AnimationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (load_player_sprites, load_creature_sprites, load_npc_sprites));
        app.add_systems(
            Update,
            (
                sync_local_weapon_type,
                // Needs this frame's own `AimIndicator`/`WeaponTypeIndicator`
                // (see `animate_players`' own doc for why those are what
                // decide the charging-hold/aimed-direction/weapon-specific
                // clip behavior) already synced from whichever of the
                // local-prediction path or the latest snapshot applies,
                // not last frame's.
                animate_players
                    .after(crate::net::apply_remote_snapshots)
                    .after(crate::aim_display::sync_local_aim)
                    .after(sync_local_weapon_type),
                animate_creatures,
                animate_npcs,
                animate_objects,
            ),
        );
    }
}

/// Plays one animation's cue exactly once -- called only from the same
/// "animation just changed" edge both `animate_players` and
/// `animate_creatures` already detect (`anim.last_kind != Some(kind)`),
/// never every frame the animation continues to play. A no-op for a
/// `kind` with no authored cue (`None`). Spawned as its own short-lived
/// entity (`PlaybackSettings::DESPAWN` cleans it up once playback ends)
/// rather than attached to the character entity itself -- a character
/// can restart the very same animation (e.g. attack again) before the
/// previous cue finishes, and each play needs its own independent
/// lifetime, not one shared slot fighting itself.
fn play_anim_sound(commands: &mut Commands, sounds: &[Handle<AudioSource>]) {
    // Picks uniformly at random among however many variants this
    // animation has -- see `AnimSounds`'s own doc for why more than one
    // exists at all. `gen_range` needs a non-empty range, hence the
    // separate `len() == 1` case rather than always calling it.
    let source = match sounds.len() {
        0 => return,
        1 => &sounds[0],
        len => &sounds[rand::thread_rng().gen_range(0..len)],
    };
    commands.spawn(AudioBundle {
        source: source.clone(),
        settings: PlaybackSettings::DESPAWN,
    });
}
