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

use std::collections::HashMap;

use bevy::prelude::*;
use rand::Rng;

use game_core::components::{Airborne, Creature, Facing, Npc, Player};
use game_core::creature::{CreatureId, CreatureRegistry};
use game_core::npc::{NpcId, NpcRegistry};
use game_core::states::CombatState;

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

/// PixelLab's own export format (`gallery/<character>/metadata.json`),
/// not this project's usual RON convention -- only the part actually
/// used, `frames.animations.<AnimName>.<direction>`, is declared; every
/// other field (character prompt, rotations, export_date, ...) is
/// present in the file but simply ignored by serde. One character can
/// have multiple `states` entries (a leftover of however the export tool
/// batches its runs) -- `frame_paths` searches all of them.
#[derive(serde::Deserialize)]
struct SpriteMetadata {
    states: Vec<SpriteMetadataState>,
}

#[derive(serde::Deserialize)]
struct SpriteMetadataState {
    frames: SpriteMetadataFrames,
}

#[derive(serde::Deserialize)]
struct SpriteMetadataFrames {
    #[serde(default)]
    animations: HashMap<String, SpriteMetadataAnimation>,
}

/// One entry in `metadata.json`'s own `frames.animations` map. PixelLab's
/// own export only ever writes the 8 direction keys (each a frame-path
/// list); `sound` is this project's own addition -- a hand-added sibling
/// key naming this animation's one-shot cue (see this module's own doc
/// for where that gets played). `#[serde(flatten)]` on `directions` is
/// what lets one JSON object mix that one known field with the direction
/// keys' otherwise-arbitrary names, instead of `sound` needing its own
/// nesting level that would break PixelLab's own export shape.
#[derive(serde::Deserialize)]
struct SpriteMetadataAnimation {
    #[serde(default)]
    sound: Option<String>,
    #[serde(flatten)]
    directions: HashMap<String, Vec<String>>,
}

impl SpriteMetadata {
    /// The exact frame paths (relative to the character's own folder,
    /// e.g. `"animations/Idle/east/frame_000.png"`) metadata.json records
    /// for one animation/direction pair, if it describes that pair at
    /// all -- `load_direction_frames` falls back to a filesystem scan
    /// when this returns `None`, since not every animation a character
    /// has is necessarily in its metadata (see that function's doc).
    fn frame_paths(&self, animation: &str, direction: &str) -> Option<Vec<String>> {
        self.states.iter().find_map(|s| s.frames.animations.get(animation)?.directions.get(direction).cloned())
    }

    /// This animation's one-shot sound cue path (relative to the
    /// character's own folder, e.g. `"sounds/Running.mp3"`), if a
    /// `"sound"` key is present -- see `SpriteMetadataAnimation`'s own
    /// doc. `load_animation_sound` falls back to a direct file check when
    /// this returns `None`, same "metadata first, filesystem scan as
    /// fallback" story `frame_paths` already has.
    fn sound_path(&self, animation: &str) -> Option<String> {
        self.states.iter().find_map(|s| s.frames.animations.get(animation)?.sound.clone())
    }
}

/// Reads `gallery/<base_path>/metadata.json` straight off disk, like
/// `client::map`'s own zone-file loading -- has to happen synchronously,
/// before the `asset_server.load` calls below can even know which paths
/// to ask for, so this bypasses Bevy's (asynchronous) asset pipeline on
/// purpose. `None` if the character has no metadata.json at all (nothing
/// requires one -- `load_direction_frames` scans the filesystem directly
/// in that case) or it fails to parse.
fn load_metadata(base_path: &str) -> Option<SpriteMetadata> {
    let contents = std::fs::read_to_string(format!("gallery/{base_path}/metadata.json")).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Every `frame_NNN.png` actually present in
/// `gallery/<base_path>/animations/<animation>/<direction>/`, sorted --
/// the fallback `load_direction_frames` uses when `metadata.json` doesn't
/// describe this animation/direction (missing entirely, or -- like
/// `gallery/animals/sheep/metadata.json`, which predates its own Dying
/// animation -- exported before this animation existed). Scanning the
/// real folder instead of assuming a count means a character/creature
/// with no metadata.json at all still loads correctly, just without the
/// authoritative-count benefit metadata gives everyone else.
fn scan_frame_paths(base_path: &str, animation: &str, direction: &str) -> Vec<String> {
    let dir = format!("gallery/{base_path}/animations/{animation}/{direction}");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with("frame_") && name.ends_with(".png"))
        .collect();
    names.sort();
    names.into_iter().map(|name| format!("animations/{animation}/{direction}/{name}")).collect()
}

/// One animation's full 8-direction frame set. Prefers `metadata`'s own
/// record of which frames exist (see `SpriteMetadata::frame_paths`) so a
/// character with more or fewer frames than another just works with no
/// code change; falls back to scanning the folder directly when metadata
/// doesn't cover this animation/direction (see `scan_frame_paths`).
///
/// `animation_names` is tried in order, first match wins per direction --
/// lets a caller offer synonyms (e.g. a creature's `["Running",
/// "Walking"]`) instead of every asset needing to agree on one exact
/// name; PixelLab's own animal exports have used "Walking" for a
/// ground-movement cycle where a human character's used "Running".
///
/// If *no* candidate name has any frames for a direction at all (a
/// creature authored with only some animations, e.g. one with just
/// Walking+Dying and no Idle), falls back to the single static
/// `rotations/<direction>.png` every character already has, used as a
/// one-frame "animation" -- better than an empty frame list, which
/// `animate_players`/`animate_creatures`'s own `frame % frames.len()`
/// would divide-by-zero on the instant that direction/animation is ever
/// actually played (not a rare edge case: `CombatState::Idle` is reached
/// on essentially every spawn and every wander-leg transition, regardless
/// of how brief).
fn load_direction_frames(
    asset_server: &AssetServer,
    base_path: &str,
    animation_names: &[&str],
    metadata: Option<&SpriteMetadata>,
) -> DirectionFrames {
    std::array::from_fn(|dir| {
        let direction = DIRECTION_FOLDERS[dir];
        let found = animation_names.iter().find_map(|animation| {
            let paths = metadata
                .and_then(|m| m.frame_paths(animation, direction))
                .unwrap_or_else(|| scan_frame_paths(base_path, animation, direction));
            (!paths.is_empty()).then_some(paths)
        });
        match found {
            Some(paths) => paths.into_iter().map(|path| asset_server.load(format!("{base_path}/{path}"))).collect(),
            None => {
                let rotation_file = format!("gallery/{base_path}/rotations/{direction}.png");
                if std::path::Path::new(&rotation_file).exists() {
                    vec![asset_server.load(format!("{base_path}/rotations/{direction}.png"))]
                } else {
                    Vec::new()
                }
            }
        }
    })
}

/// This animation's sound cue variants, if it has any -- mirrors
/// `load_direction_frames`'s own "metadata first, filesystem scan as
/// fallback" story and its `animation_names` synonym list. Checked in
/// order, first match wins (never merged across sources):
///
/// 1. A single explicit `metadata.json` `"sound"` entry (unchanged from
///    before variants existed) -- always exactly one handle.
/// 2. `gallery/<base_path>/sounds/<AnimName>/` as a *folder* -- every
///    file inside is one variant, `play_anim_sound` picks one at random
///    each time this animation starts. Mirrors the exact convention
///    `animations/<AnimName>/<direction>/frame_NNN.png` already uses
///    (one folder per concept, files inside are what varies) rather than
///    inventing a second naming scheme.
/// 3. `gallery/<base_path>/sounds/<AnimName>.mp3` as a single flat file
///    -- the original convention (`gallery/animals/hen_king/sounds/`
///    already uses this), kept working as-is so a creature with just one
///    cue never needs migrating into a folder.
///
/// An empty `Vec` (every animation that hasn't had a cue authored for it
/// at all) means "silent" -- `play_anim_sound` is simply a no-op for a
/// `kind` with no entries here, exactly as before variants existed.
fn load_animation_sounds(
    asset_server: &AssetServer,
    base_path: &str,
    animation_names: &[&str],
    metadata: Option<&SpriteMetadata>,
) -> Vec<Handle<AudioSource>> {
    for animation in animation_names {
        if let Some(path) = metadata.and_then(|m| m.sound_path(animation)) {
            return vec![asset_server.load(format!("{base_path}/{path}"))];
        }

        let variants_dir = format!("gallery/{base_path}/sounds/{animation}");
        if let Ok(entries) = std::fs::read_dir(&variants_dir) {
            let mut file_names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|name| is_audio_file(name))
                .collect();
            if !file_names.is_empty() {
                // Sorted purely for deterministic asset-load order across
                // runs -- playback order is `play_anim_sound`'s own
                // random pick, not this Vec's order.
                file_names.sort();
                return file_names
                    .into_iter()
                    .map(|name| asset_server.load(format!("{base_path}/sounds/{animation}/{name}")))
                    .collect();
            }
        }

        let direct = format!("gallery/{base_path}/sounds/{animation}.mp3");
        if std::path::Path::new(&direct).exists() {
            return vec![asset_server.load(format!("{base_path}/sounds/{animation}.mp3"))];
        }
    }
    Vec::new()
}

/// Extension allowlist for `load_animation_sounds`'s folder scan -- every
/// format this project's own audio decoding actually supports (see
/// `client/Cargo.toml`'s own `bevy` feature list), so a stray non-audio
/// file dropped in the same folder (a `.txt` note, a `.psd` source) isn't
/// mistaken for a playable variant.
fn is_audio_file(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower.ends_with(".mp3") || lower.ends_with(".ogg") || lower.ends_with(".wav")
}

fn load_player_sprites(mut commands: Commands, asset_server: Res<AssetServer>) {
    // "human" -- the default race's own 64x64 art set, used as every
    // player's own sprite for now regardless of `CharacterRace` (see
    // `PlayerSprites`'s own doc for why -- a per-race set, one
    // `PlayerSprites`-like resource per `RaceId` mirroring
    // `CreatureSprites`' own per-`CreatureId` shape, is still the natural
    // eventual follow-up). "elf" (32x32) was tried here briefly but read
    // as too small on screen; back to "human" until per-race sprites
    // exist for real.
    let base_path = "characters/human";
    let metadata = load_metadata(base_path);
    let death =
        std::array::from_fn(|dir| asset_server.load(format!("{base_path}/death/{}.png", DIRECTION_FOLDERS[dir])));
    // Load the generic Attacking clip first -- every weapon-specific one
    // below falls back to it by name (`["Attacking_*", "Attacking"]`,
    // first match per direction wins -- see `load_direction_frames`' own
    // doc), so a character with no dedicated art for a given weapon type
    // (every race but elf, today) still shows *something* recognizable
    // as a swing instead of a static rotation frame.
    let attacking = load_direction_frames(&asset_server, base_path, &["Attacking"], metadata.as_ref());
    commands.insert_resource(PlayerSprites {
        idle: load_direction_frames(&asset_server, base_path, &["Idle"], metadata.as_ref()),
        running: load_direction_frames(&asset_server, base_path, &["Running"], metadata.as_ref()),
        jumping: load_direction_frames(&asset_server, base_path, &["Jumping"], metadata.as_ref()),
        attacking_sword: load_direction_frames(&asset_server, base_path, &["Attacking_sword", "Attacking"], metadata.as_ref()),
        attacking_bow: load_direction_frames(&asset_server, base_path, &["Attacking_bow", "Attacking"], metadata.as_ref()),
        attacking_spear: load_direction_frames(&asset_server, base_path, &["Attacking_spear", "Attacking"], metadata.as_ref()),
        casting: load_direction_frames(&asset_server, base_path, &["Casting", "Attacking"], metadata.as_ref()),
        dying: load_direction_frames(&asset_server, base_path, &["Dying"], metadata.as_ref()),
        falling: load_direction_frames(&asset_server, base_path, &["Falling"], metadata.as_ref()),
        pushing: load_direction_frames(&asset_server, base_path, &["Pushing", "Running"], metadata.as_ref()),
        death,
        sounds: AnimSounds {
            idle: load_animation_sounds(&asset_server, base_path, &["Idle"], metadata.as_ref()),
            running: load_animation_sounds(&asset_server, base_path, &["Running"], metadata.as_ref()),
            attacking: load_animation_sounds(&asset_server, base_path, &["Attacking"], metadata.as_ref()),
            dying: load_animation_sounds(&asset_server, base_path, &["Dying"], metadata.as_ref()),
            casting: load_animation_sounds(&asset_server, base_path, &["Casting"], metadata.as_ref()),
            pushing: load_animation_sounds(&asset_server, base_path, &["Pushing"], metadata.as_ref()),
        },
        attacking,
    });
}

fn load_creature_sprites(mut commands: Commands, asset_server: Res<AssetServer>, registry: Res<CreatureRegistry>) {
    let sets = registry
        .creatures
        .iter()
        .map(|(id, def)| {
            let base_path = format!("{}/{id}", def.sprite_category);
            let metadata = load_metadata(&base_path);
            let death = std::array::from_fn(|dir| {
                asset_server.load(format!("{base_path}/death/{}.png", DIRECTION_FOLDERS[dir]))
            });
            let set = CreatureAnimSet {
                idle: load_direction_frames(&asset_server, &base_path, &["Idle"], metadata.as_ref()),
                // "Walking" as a fallback name -- PixelLab's own animal
                // exports (e.g. the hen's) have used that instead of
                // "Running" for a ground-movement cycle; see
                // load_direction_frames' own doc.
                running: load_direction_frames(&asset_server, &base_path, &["Running", "Walking"], metadata.as_ref()),
                attacking: load_direction_frames(&asset_server, &base_path, &["Attacking"], metadata.as_ref()),
                dying: load_direction_frames(&asset_server, &base_path, &["Dying"], metadata.as_ref()),
                death,
                sounds: AnimSounds {
                    idle: load_animation_sounds(&asset_server, &base_path, &["Idle"], metadata.as_ref()),
                    running: load_animation_sounds(&asset_server, &base_path, &["Running", "Walking"], metadata.as_ref()),
                    attacking: load_animation_sounds(&asset_server, &base_path, &["Attacking"], metadata.as_ref()),
                    dying: load_animation_sounds(&asset_server, &base_path, &["Dying"], metadata.as_ref()),
                    // Creatures never cast or push -- see `AnimKind::
                    // Casting`/`Pushing`'s own docs, both player-only.
                    ..default()
                },
            };
            (id.clone(), set)
        })
        .collect();
    commands.insert_resource(CreatureSprites { sets });
}

/// Every `.png` actually present in
/// `gallery/<base_path>/<animation>/<direction>/`, sorted. Deliberately
/// its own function rather than a call to `scan_frame_paths`: an NPC's
/// own export uses a different folder shape than a creature's --
/// `Idle`/`Running` sit directly under the NPC's own folder, no
/// intervening `animations/` prefix. Filenames aren't assumed to follow
/// any particular pattern beyond "sorts into the right playback order"
/// on purpose -- Lucas's own first export used non-sequential
/// PixelLab keyframe indices (`Idle_A_0_001.png`, `_006`, `_010`, ...,
/// the tool's own keyframe numbers, not a frame count) before being
/// cleaned up to the same zero-padded `frame_NNN.png` convention
/// creatures use; both sort correctly with a plain string sort (zero-
/// padded numeric suffixes sort lexicographically in the same order
/// they'd sort numerically), so this never needed to change either way.
/// No `metadata.json`/synonym-name support either -- an NPC's export
/// doesn't have one and only ever has exactly one candidate name per
/// animation, unlike a creature's `["Running", "Walking"]`.
fn scan_npc_frame_paths(base_path: &str, animation: &str, direction: &str) -> Vec<String> {
    let dir = format!("gallery/{base_path}/{animation}/{direction}");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.to_ascii_lowercase().ends_with(".png"))
        .collect();
    names.sort();
    names.into_iter().map(|name| format!("{animation}/{direction}/{name}")).collect()
}

fn load_npc_direction_frames(asset_server: &AssetServer, base_path: &str, animation: &str) -> DirectionFrames {
    std::array::from_fn(|dir| {
        let direction = DIRECTION_FOLDERS[dir];
        scan_npc_frame_paths(base_path, animation, direction)
            .into_iter()
            .map(|path| asset_server.load(format!("{base_path}/{path}")))
            .collect()
    })
}

/// One `NpcAnimSet` per entry in `NpcRegistry`, from
/// `gallery/npc/<sprite_path>/{Idle,Running}/<direction>/...` -- see
/// `scan_npc_frame_paths`'s own doc for why this doesn't go through
/// `load_direction_frames`/`scan_frame_paths` the way a creature's own
/// sprites do.
fn load_npc_sprites(mut commands: Commands, asset_server: Res<AssetServer>, registry: Res<NpcRegistry>) {
    let sets = registry
        .npcs
        .iter()
        .map(|(id, def)| {
            let base_path = format!("npc/{}", def.sprite_path);
            let set = NpcAnimSet {
                idle: load_npc_direction_frames(&asset_server, &base_path, "Idle"),
                running: load_npc_direction_frames(&asset_server, &base_path, "Running"),
            };
            (id.clone(), set)
        })
        .collect();
    commands.insert_resource(NpcSprites { sets });
}

/// The `animate_creatures` equivalent for `Npc` entities -- much
/// simpler, since there's only ever Idle or Running to choose between
/// (see `NpcAnimSet`'s own doc: no attack, no death). Reuses `RUN_FPS`/
/// `IDLE_FPS` outright rather than inventing NPC-specific rates, for the
/// same "reads as part of the same world" reason every other walking
/// thing in this client shares those two constants.
fn animate_npcs(
    sprites: Option<Res<NpcSprites>>,
    time: Res<Time>,
    mut query: Query<(&Npc, &Facing, &CombatState, &mut AnimationState, &mut Handle<Image>)>,
) {
    let Some(sprites) = sprites else { return };

    for (npc, facing, state, mut anim, mut texture) in &mut query {
        let Some(set) = sprites.sets.get(&npc.0) else { continue };
        let dir = *facing as usize;

        let kind = if *state == CombatState::Moving { AnimKind::Running } else { AnimKind::Idle };
        if anim.last_kind != Some(kind) {
            anim.frame = 0;
            anim.elapsed = 0.0;
            anim.last_kind = Some(kind);
        }

        let (frames, fps) = match kind {
            AnimKind::Running => (&set.running[dir], RUN_FPS),
            _ => (&set.idle[dir], IDLE_FPS),
        };
        // No art at all for this animation/direction -- leave whatever
        // texture was already showing rather than divide-by-zero on
        // frames.len(), same guard `animate_creatures` has.
        if frames.is_empty() {
            continue;
        }

        anim.elapsed += time.delta_seconds();
        let frame_time = 1.0 / fps;
        while anim.elapsed >= frame_time {
            anim.elapsed -= frame_time;
            anim.frame = (anim.frame + 1) % frames.len();
        }
        *texture = frames[anim.frame].clone();
    }
}

/// Local-player-only: mirrors the live `Equipment`/`ItemRegistry` lookup
/// onto this entity's own `WeaponTypeIndicator`, so `animate_players` can
/// treat the local player exactly like a remote one (whose own
/// `WeaponTypeIndicator` instead comes from `client::net::
/// apply_remote_snapshots` reading `protocol::EntitySnapshot::
/// weapon_type` -- a remote entity has no real `Equipment` of its own to
/// read locally). Same "zero latency for the one player who can see the
/// difference instantly" reasoning `client::charge_display`'s own local
/// sync already has.
fn sync_local_weapon_type(
    local_player: Option<Res<crate::net::LocalPlayer>>,
    items: Res<game_core::item::ItemRegistry>,
    mut query: Query<(&mut WeaponTypeIndicator, Option<&game_core::components::Equipment>)>,
) {
    let Some(local_player) = local_player else { return };
    let Ok((mut indicator, equipped)) = query.get_mut(local_player.entity) else { return };
    indicator.0 = equipped
        .and_then(|eq| eq.weapon(&items))
        .and_then(|(_, item_id)| items.items.get(item_id))
        .and_then(|def| def.weapon_type.clone());
}

fn animate_players(
    mut commands: Commands,
    sprites: Option<Res<PlayerSprites>>,
    time: Res<Time>,
    mut query: Query<
        (
            &Facing,
            &CombatState,
            Option<&Airborne>,
            Option<&crate::aim_display::AimIndicator>,
            Option<&crate::cast_circle_display::CastingAbilityId>,
            Option<&WeaponTypeIndicator>,
            Option<&game_core::components::Pushing>,
            &mut AnimationState,
            &mut Handle<Image>,
        ),
        With<Player>,
    >,
) {
    // Sprites load asynchronously; skip the handful of frames before
    // load_player_sprites' Commands have actually been applied.
    let Some(sprites) = sprites else { return };

    for (facing, state, airborne, aim, casting_ability, weapon_type, is_pushing, mut anim, mut texture) in &mut query {
        let dir = *facing as usize;

        // Dead wins over everything, same as `animate_creatures`: play
        // the Dying clip once, then hold on the dedicated static `death`
        // image forever (not the last Dying frame, still mid-collapse).
        // This entity itself stays dead until the player clicks "Revive"
        // on `client::death_screen`'s own prompt (`systems::respawn::
        // tick_respawn` then teleports it away) -- the lingering corpse
        // a player actually sees afterward is a separate, persistent
        // entity `server::loot::spawn_player_corpses` leaves behind,
        // rendered through this exact same path (it's `EntityKind::
        // Player`-tagged and `CombatState::Dead` forever, see that
        // function's own doc).
        if *state == CombatState::Dead {
            if anim.last_kind != Some(AnimKind::Dying) {
                anim.frame = 0;
                anim.elapsed = 0.0;
                anim.last_kind = Some(AnimKind::Dying);
                play_anim_sound(&mut commands, sprites.sounds.get(AnimKind::Dying));
            }

            let dying_frames = &sprites.dying[dir];
            let last_frame = dying_frames.len().saturating_sub(1);
            if anim.frame >= last_frame {
                *texture = sprites.death[dir].clone();
                continue;
            }

            anim.elapsed += time.delta_seconds();
            let frame_time = 1.0 / DYING_FPS;
            while anim.elapsed >= frame_time {
                anim.elapsed -= frame_time;
                anim.frame = (anim.frame + 1).min(last_frame);
            }
            *texture = dying_frames[anim.frame].clone();
            continue;
        }

        // Recovering (just fell through a floor gap, see
        // `game_core::components::FallRecoveryTimer`) plays the Falling
        // clip once, then holds on its own *last* frame -- unlike Dying,
        // there's no dedicated static "landed" image, so the clip's last
        // frame doubles as the resting pose for however much of the
        // lockout remains once it's played through.
        if *state == CombatState::Recovering {
            if anim.last_kind != Some(AnimKind::Falling) {
                anim.frame = 0;
                anim.elapsed = 0.0;
                anim.last_kind = Some(AnimKind::Falling);
                play_anim_sound(&mut commands, sprites.sounds.get(AnimKind::Falling));
            }

            let falling_frames = &sprites.falling[dir];
            let last_frame = falling_frames.len().saturating_sub(1);
            anim.elapsed += time.delta_seconds();
            let frame_time = 1.0 / FALLING_FPS;
            while anim.elapsed >= frame_time {
                anim.elapsed -= frame_time;
                anim.frame = (anim.frame + 1).min(last_frame);
            }
            *texture = falling_frames[anim.frame].clone();
            continue;
        }

        // Bow charging holds whichever attack-family clip applies open at
        // its own middle frame for as long as the draw lasts, instead of
        // a swing's fixed duration -- see `client::aim_display::
        // AimIndicator`'s own doc for why *that* (not a live
        // `CombatState`/`ChargingAttack` check) is the right thing to
        // read here: it already resolves "is this specifically a bow
        // draw, not a spell cast" the same way for a local *and* a
        // remote player, no extra lookup needed.
        let charging_bow = aim.is_some_and(|a| a.visible);
        let is_ability = casting_ability.is_some_and(|c| c.0.is_some());
        let weapon_type_str = weapon_type.and_then(|w| w.0.as_deref());

        // Which of the five `AnimKind::is_attack_family` clips applies
        // *right now*, if any -- a fresh choice while genuinely
        // charging/attacking (`Casting` whenever this is an ability at
        // all, since a spell has no weapon backing it to pick a
        // weapon-specific clip from; otherwise whichever of `sword`/
        // `bow`/`spear` the equipped weapon's own `weapon_type` names,
        // falling back to the plain `Attacking` clip for anything else --
        // unarmed, an unrecognized type, or no `WeaponTypeIndicator` at
        // all yet). Once neither condition holds any more, falls back to
        // *continuing* whatever was already playing (`anim.last_kind`)
        // rather than cutting off immediately -- a bow/ability release's
        // own live `CombatState::Attacking` is typically alive for a
        // single tick, nowhere near long enough for a clip's second half
        // to actually render off of it directly (see `game_core::
        // systems::combat::tick_bow_charging`'s own doc) -- but only for
        // as long as that clip genuinely hasn't reached its own last
        // frame yet; once it has, this correctly yields `None` and
        // control falls through to the ordinary Idle/Running/Jumping/
        // Pushing selection below.
        let attack_kind: Option<AnimKind> = if charging_bow || matches!(*state, CombatState::Attacking { .. }) {
            Some(if is_ability {
                AnimKind::Casting
            } else {
                match weapon_type_str {
                    Some("sword") => AnimKind::AttackingSword,
                    Some("bow") => AnimKind::AttackingBow,
                    Some("spear") => AnimKind::AttackingSpear,
                    _ => AnimKind::Attacking,
                }
            })
        } else {
            anim.last_kind.filter(|&k| k.is_attack_family()).filter(|&k| {
                let last_frame = sprites.attack_frames(k)[dir].len().saturating_sub(1);
                anim.frame < last_frame
            })
        };

        // Attacking (charging, mid-swing, or finishing any of the above)
        // wins over everything remaining, including a jump in progress
        // (no air-attack rule exists, so this just means you can't
        // jump-cancel out of a swing today).
        if let Some(kind) = attack_kind {
            // While actively charging, the sprite points wherever the
            // shot is currently aimed instead of `Facing` -- bucketed to
            // the same 8 compass directions `Facing` itself uses
            // (`Facing::from_angle_radians` does exactly this bucketing).
            // Direction can change every frame without ever restarting
            // the clip: `anim.frame`/`elapsed` are untouched by this,
            // only which of the 8 per-direction arrays they index into --
            // rotating past a 45-degree boundary swaps to the same frame
            // position in the newly-facing set, not back to frame 0.
            // Falls back to `Facing` once released (the shot's own
            // direction is already committed server-side by then, and
            // `AimIndicator` itself goes invisible the instant release
            // happens anyway -- see that component's own doc).
            let dir = if charging_bow {
                aim.map_or(dir, |a| Facing::from_angle_radians(a.angle) as usize)
            } else {
                dir
            };

            if anim.last_kind != Some(kind) {
                anim.frame = 0;
                anim.elapsed = 0.0;
                anim.last_kind = Some(kind);
                play_anim_sound(&mut commands, sprites.sounds.get(kind));
            }

            let frames = &sprites.attack_frames(kind)[dir];
            let last_frame = frames.len().saturating_sub(1);
            // Charging holds at the clip's own middle frame indefinitely
            // (a draw has no fixed length the way a swing does); a
            // regular swing/cast -- or a release finishing its back half
            // after `CombatState` has already moved on, see `attack_kind`
            // above -- instead plays through to the real last frame once
            // and holds *there*: a swing is a one-shot, not a loop (this
            // also means an attack whose own `recovery_ticks` outlasts
            // its clip now holds on the last frame instead of visibly
            // repeating, which the swing case never used to guard
            // against).
            let cap = if charging_bow { frames.len() / 2 } else { last_frame };
            anim.elapsed += time.delta_seconds();
            let frame_time = 1.0 / ATTACK_FPS;
            while anim.elapsed >= frame_time {
                anim.elapsed -= frame_time;
                anim.frame = (anim.frame + 1).min(cap);
            }
            *texture = frames[anim.frame].clone();
            continue;
        }

        // Airborne wins over Idle/Moving/Pushing the same as always.
        // Pushing wins over plain Moving whenever both would apply --
        // `components::Pushing` is only ever true while genuinely trying
        // to move (see that component's own doc), so the two already
        // coincide; this just decides which of the two clips to show.
        let kind = if airborne.is_some_and(|a| a.height > 0.0) {
            AnimKind::Jumping
        } else if is_pushing.is_some_and(|p| p.0) {
            AnimKind::Pushing
        } else if *state == CombatState::Moving {
            AnimKind::Running
        } else {
            AnimKind::Idle
        };

        if anim.last_kind != Some(kind) {
            anim.frame = 0;
            anim.elapsed = 0.0;
            anim.last_kind = Some(kind);
            play_anim_sound(&mut commands, sprites.sounds.get(kind));
        }

        let (frames, fps) = match kind {
            AnimKind::Jumping => (&sprites.jumping[dir], JUMP_FPS),
            AnimKind::Pushing => (&sprites.pushing[dir], PUSHING_FPS),
            AnimKind::Running => (&sprites.running[dir], RUN_FPS),
            _ => (&sprites.idle[dir], IDLE_FPS),
        };

        anim.elapsed += time.delta_seconds();
        let frame_time = 1.0 / fps;
        while anim.elapsed >= frame_time {
            anim.elapsed -= frame_time;
            anim.frame = (anim.frame + 1) % frames.len();
        }
        *texture = frames[anim.frame].clone();
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

fn animate_objects(time: Res<Time>, mut query: Query<(&mut ObjectAnimation, &mut Handle<Image>)>) {
    for (mut anim, mut texture) in &mut query {
        anim.elapsed += time.delta_seconds();
        let frame_time = 1.0 / anim.fps;
        while anim.elapsed >= frame_time {
            anim.elapsed -= frame_time;
            anim.frame = (anim.frame + 1) % anim.frames.len();
        }
        *texture = anim.frames[anim.frame].clone();
    }
}

fn animate_creatures(
    mut commands: Commands,
    sprites: Option<Res<CreatureSprites>>,
    time: Res<Time>,
    mut query: Query<(&Creature, &Facing, &CombatState, &mut AnimationState, &mut Handle<Image>), With<Creature>>,
) {
    let Some(sprites) = sprites else { return };

    for (creature, facing, state, mut anim, mut texture) in &mut query {
        let Some(set) = sprites.sets.get(&creature.0) else { continue };
        let dir = *facing as usize;

        if *state == CombatState::Dead {
            if anim.last_kind != Some(AnimKind::Dying) {
                anim.frame = 0;
                anim.elapsed = 0.0;
                anim.last_kind = Some(AnimKind::Dying);
                play_anim_sound(&mut commands, set.sounds.get(AnimKind::Dying));
            }

            let dying_frames = &set.dying[dir];
            let last_frame = dying_frames.len().saturating_sub(1);
            if anim.frame >= last_frame {
                // Played through once -- hold on the dedicated corpse
                // image from here on, not the last Dying frame (which is
                // still mid-collapse, not a resting pose).
                *texture = set.death[dir].clone();
                continue;
            }

            anim.elapsed += time.delta_seconds();
            let frame_time = 1.0 / DYING_FPS;
            while anim.elapsed >= frame_time {
                anim.elapsed -= frame_time;
                anim.frame = (anim.frame + 1).min(last_frame);
            }
            *texture = dying_frames[anim.frame].clone();
            continue;
        }

        let kind = if matches!(*state, CombatState::Attacking { .. }) {
            AnimKind::Attacking
        } else if *state == CombatState::Moving {
            AnimKind::Running
        } else {
            AnimKind::Idle
        };

        if anim.last_kind != Some(kind) {
            anim.frame = 0;
            anim.elapsed = 0.0;
            anim.last_kind = Some(kind);
            play_anim_sound(&mut commands, set.sounds.get(kind));
        }

        let (frames, fps) = match kind {
            AnimKind::Attacking => (&set.attacking[dir], CREATURE_ATTACK_FPS),
            AnimKind::Running => (&set.running[dir], RUN_FPS),
            _ => (&set.idle[dir], IDLE_FPS),
        };

        // No art at all for this animation/direction -- not even the
        // rotations/ fallback load_direction_frames tries first (a
        // creature missing its own rotations/<direction>.png entirely).
        // Leave whatever texture was already showing rather than
        // divide-by-zero on frames.len().
        if frames.is_empty() {
            continue;
        }

        anim.elapsed += time.delta_seconds();
        let frame_time = 1.0 / fps;
        while anim.elapsed >= frame_time {
            anim.elapsed -= frame_time;
            anim.frame = (anim.frame + 1) % frames.len();
        }
        *texture = frames[anim.frame].clone();
    }
}
