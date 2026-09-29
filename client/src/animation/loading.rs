//! Loading sprite sets and their sounds from gallery/ at startup: frames
//! per direction, sprite metadata, sound files.

use std::collections::HashMap;

use bevy::prelude::*;

use game_core::creature::CreatureRegistry;
use game_core::npc::NpcRegistry;

use super::{
    AnimSounds, CreatureAnimSet, CreatureSprites, DIRECTION_FOLDERS, DirectionFrames, NpcAnimSet, NpcSprites,
    PlayerSprites,
};

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

pub(super) fn load_player_sprites(mut commands: Commands, asset_server: Res<AssetServer>) {
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

pub(super) fn load_creature_sprites(mut commands: Commands, asset_server: Res<AssetServer>, registry: Res<CreatureRegistry>) {
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
pub(super) fn load_npc_sprites(mut commands: Commands, asset_server: Res<AssetServer>, registry: Res<NpcRegistry>) {
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
