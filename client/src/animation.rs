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
//! code change here). `animate_creatures` also owns death: once
//! `CombatState::Dead`, it plays the Dying animation exactly once (no
//! looping) and then holds on a dedicated static corpse image forever --
//! nothing here ever despawns a dead creature, that's left for whatever
//! "the body gets used/eaten" mechanic comes later.

use std::collections::HashMap;

use bevy::prelude::*;
use rand::Rng;

use game_core::components::{Airborne, Creature, Facing, Player};
use game_core::creature::{CreatureId, CreatureRegistry};
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
/// `gallery/animals/<id>/animations/Dying` has 9 frames -- picked so the
/// animation takes a little under a second, long enough to actually read
/// as a death rather than a flinch.
const DYING_FPS: f32 = 10.0;

/// Which loaded animation is currently playing. `Jumping` is only ever
/// produced for players -- no creature jumps. `Attacking` is produced by
/// both `animate_players` and `animate_creatures`, each reading its own
/// `CombatState::Attacking`. `Dying` is only ever produced for
/// creatures -- see `systems::respawn` (`game_core`) for how a player's
/// own death is handled instead (a timed revive, not a played-through
/// death animation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimKind {
    Idle,
    Running,
    Jumping,
    Attacking,
    Dying,
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

/// All textures for the one test character, preloaded once at startup.
#[derive(Resource)]
pub struct PlayerSprites {
    idle: DirectionFrames,
    running: DirectionFrames,
    jumping: DirectionFrames,
    attacking: DirectionFrames,
    sounds: AnimSounds,
}

/// This animation's sound cue *variants* (see `load_animation_sounds`),
/// resolved once at load time the same way its matching `DirectionFrames`
/// set is. Almost always 0 or 1 entries; more than one means
/// `play_anim_sound` picks a random one each time this animation starts,
/// so e.g. a whole field of sheep don't all bleat in exact unison. Kept
/// as named fields rather than a `HashMap<AnimKind, _>` since only these
/// 4 kinds ever apply to a player or creature (`Dying`'s own player-side
/// sound would live here too if player death ever got a played animation
/// instead of `systems::respawn`'s timed revive -- see `AnimKind`'s own
/// doc).
#[derive(Default)]
struct AnimSounds {
    idle: Vec<Handle<AudioSource>>,
    running: Vec<Handle<AudioSource>>,
    attacking: Vec<Handle<AudioSource>>,
    dying: Vec<Handle<AudioSource>>,
}

impl AnimSounds {
    fn get(&self, kind: AnimKind) -> &[Handle<AudioSource>] {
        match kind {
            AnimKind::Idle => &self.idle,
            AnimKind::Running => &self.running,
            AnimKind::Attacking => &self.attacking,
            AnimKind::Dying => &self.dying,
            AnimKind::Jumping => &[],
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
    /// A starting state for a creature entity that's already dead the
    /// moment it's spawned -- see `net::apply_remote_snapshots`'s own
    /// doc for why this has to exist: a creature that died while out of
    /// the local player's vision gets despawned, then a *brand new*
    /// entity (with a brand new, default `AnimationState`) once it
    /// re-enters vision. `AnimationState::default()`'s `last_kind: None`
    /// reads to `animate_creatures` as "just started dying this frame",
    /// replaying the entire death animation on what should already be a
    /// motionless corpse. `frame` is set past any real animation's
    /// length so `animate_creatures`'s own "already played through
    /// once" check trips immediately, skipping straight to the resting
    /// corpse image instead of frame 0.
    pub fn already_dead() -> Self {
        Self { frame: usize::MAX, elapsed: 0.0, last_kind: Some(AnimKind::Dying) }
    }
}

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
        app.add_systems(Startup, (load_player_sprites, load_creature_sprites));
        app.add_systems(Update, (animate_players, animate_creatures, animate_objects));
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
    let base_path = "characters/test_player";
    let metadata = load_metadata(base_path);
    commands.insert_resource(PlayerSprites {
        idle: load_direction_frames(&asset_server, base_path, &["Idle"], metadata.as_ref()),
        running: load_direction_frames(&asset_server, base_path, &["Running"], metadata.as_ref()),
        jumping: load_direction_frames(&asset_server, base_path, &["Jumping"], metadata.as_ref()),
        attacking: load_direction_frames(&asset_server, base_path, &["Attacking"], metadata.as_ref()),
        sounds: AnimSounds {
            idle: load_animation_sounds(&asset_server, base_path, &["Idle"], metadata.as_ref()),
            running: load_animation_sounds(&asset_server, base_path, &["Running"], metadata.as_ref()),
            attacking: load_animation_sounds(&asset_server, base_path, &["Attacking"], metadata.as_ref()),
            dying: Vec::new(),
        },
    });
}

fn load_creature_sprites(mut commands: Commands, asset_server: Res<AssetServer>, registry: Res<CreatureRegistry>) {
    let sets = registry
        .creatures
        .keys()
        .map(|id| {
            let base_path = format!("animals/{id}");
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
                },
            };
            (id.clone(), set)
        })
        .collect();
    commands.insert_resource(CreatureSprites { sets });
}

fn animate_players(
    mut commands: Commands,
    sprites: Option<Res<PlayerSprites>>,
    time: Res<Time>,
    mut query: Query<
        (&Facing, &CombatState, Option<&Airborne>, &mut AnimationState, &mut Handle<Image>),
        With<Player>,
    >,
) {
    // Sprites load asynchronously; skip the handful of frames before
    // load_player_sprites' Commands have actually been applied.
    let Some(sprites) = sprites else { return };

    for (facing, state, airborne, mut anim, mut texture) in &mut query {
        let dir = *facing as usize;

        // Attacking is a committed action -- it wins over everything,
        // including a jump in progress (no air-attack rule exists, so
        // this just means you can't jump-cancel out of a swing today).
        // Airborne otherwise wins over Idle/Moving the same as always.
        let kind = if matches!(*state, CombatState::Attacking { .. }) {
            AnimKind::Attacking
        } else if airborne.is_some_and(|a| a.height > 0.0) {
            AnimKind::Jumping
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
            AnimKind::Attacking => (&sprites.attacking[dir], ATTACK_FPS),
            AnimKind::Jumping => (&sprites.jumping[dir], JUMP_FPS),
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
