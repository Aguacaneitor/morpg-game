//! Animating players, local and remote: picking the clip from their state
//! and weapon, and its sounds.

use bevy::prelude::*;

use game_core::components::{Airborne, Facing, Player};
use game_core::states::CombatState;

use super::{
    ATTACK_FPS, AnimKind, AnimationState, DYING_FPS, FALLING_FPS, IDLE_FPS, JUMP_FPS, PUSHING_FPS, PlayerSprites,
    RUN_FPS, WeaponTypeIndicator, play_anim_sound,
};

/// Local-player-only: mirrors the live `Equipment`/`ItemRegistry` lookup
/// onto this entity's own `WeaponTypeIndicator`, so `animate_players` can
/// treat the local player exactly like a remote one (whose own
/// `WeaponTypeIndicator` instead comes from `client::net::
/// apply_remote_snapshots` reading `protocol::EntitySnapshot::
/// weapon_type` -- a remote entity has no real `Equipment` of its own to
/// read locally). Same "zero latency for the one player who can see the
/// difference instantly" reasoning `client::charge_display`'s own local
/// sync already has.
pub(super) fn sync_local_weapon_type(
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

pub(super) fn animate_players(
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
                texture.set_if_neq(sprites.death[dir].clone());
                continue;
            }

            anim.elapsed += time.delta_seconds();
            let frame_time = 1.0 / DYING_FPS;
            while anim.elapsed >= frame_time {
                anim.elapsed -= frame_time;
                anim.frame = (anim.frame + 1).min(last_frame);
            }
            texture.set_if_neq(dying_frames[anim.frame].clone());
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
            texture.set_if_neq(falling_frames[anim.frame].clone());
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
            texture.set_if_neq(frames[anim.frame].clone());
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
        texture.set_if_neq(frames[anim.frame].clone());
    }
}
