//! Applying snapshots: remote players, creatures and NPCs, the local
//! player's server corrections, and what's no longer in view.

use std::collections::HashSet;

use bevy::prelude::*;
use bevy_renet::renet::{DefaultChannel, RenetClient};

use game_core::components::{
    Airborne, Creature, Facing, Health, Hurtbox, Level, NetworkId, Npc, Player, Position, Pushing, SolidBody,
    VisionRadius,
};
use game_core::config::GameplayConfig;
use game_core::creature::CreatureRegistry;
use game_core::npc::NpcRegistry;
use game_core::states::CombatState;
use game_core::time::GameClock;
use protocol::{EntityKind, ServerMessage};

use crate::animation::AnimationState;
use crate::fade::Fade;
use crate::reconciliation::{PendingCorrection, PendingReconciliation};

use super::{INITIAL_TEXTURE, LocalPlayer, LocalPlayerMarker, NetworkHitboxes, PendingRevive, RemoteEntities, WireNames};

/// Updates every *remote* entity's (player or creature) Position from the
/// latest snapshot, spawning a sprite for any we haven't seen yet. Skips
/// our own `NetworkId` on purpose -- see `read_local_input`. For the
/// local player's own entry, this only ever *stages* a
/// `PendingReconciliation` (the server's authoritative position + which
/// of our own inputs it had already applied) -- `client::reconciliation`
/// is what actually replays and corrects `Position`, later the same
/// frame; this function never touches the local player's own `Position`
/// directly.
///
/// Also derives Facing/CombatState from the snapshot's `velocity` field.
/// Remote entities have no local `Velocity` component (see spawn comment
/// below), so `update_facing_and_movement_state` (game_core) never touches
/// them -- this is the client-only equivalent of that system, driven by
/// network data instead of a live component.
///
/// Also fades out (`crate::fade`) whichever previously-known remote
/// entities *don't* show up in this call's snapshot(s) -- `broadcast_
/// snapshots` (server) sends every visible entity fresh in every
/// snapshot, already filtered to the requester's own vision radius (see
/// `ServerMessage::Snapshot`'s own doc), so "not in this snapshot" and
/// "no longer in my vision" are the same fact. Doesn't despawn or drop
/// it from `RemoteEntities` directly -- only flips its `Fade` to fading
/// out and leaves the rest to `crate::fade`, which despawns (and *then*
/// removes the map entry) once the fade actually finishes. Keeping the
/// entity and its map entry alive for the fade's duration is what lets
/// this same code path cancel a fade-out back into a fade-in below if
/// the entity reappears before it completes, instead of the two racing
/// to spawn a second overlapping copy. Only marks entities when at least
/// one snapshot was actually received this call (`received_any`) -- an
/// Update frame with no new Unreliable packet at all carries no
/// information either way, and would otherwise be misread as "nothing is
/// visible any more" and wrongly fade out everything. The exception is an
/// entity the server says left by changing floors (`floor_exits`): it's
/// dropped at once, not faded out where it was, which would draw it on a
/// floor it has already left.
pub(crate) fn apply_remote_snapshots(
    mut commands: Commands,
    mut client: ResMut<RenetClient>,
    local_player: Res<LocalPlayer>,
    mut remotes: ResMut<RemoteEntities>,
    // Without<LocalPlayerMarker> makes this provably disjoint from any
    // future query over the local player's own entity -- the local
    // player's own entity is always skipped before remote_state would
    // ever be queried anyway (see the early continue below), but Bevy
    // can't prove that statically, so it needs the type-level guarantee
    // instead.
    mut remote_state: Query<
        (
            &mut Position,
            &mut Facing,
            &mut CombatState,
            &mut Airborne,
            &mut Health,
            &mut Level,
            Option<&mut crate::charge_display::ChargeFraction>,
            Option<&mut crate::cast_circle_display::CastingAbilityId>,
            Option<&mut crate::aim_display::AimIndicator>,
            // Nested purely to stay under Bevy's own query-tuple arity
            // limit, not for any grouping reason.
            (Option<&mut Pushing>, Option<&mut crate::animation::WeaponTypeIndicator>),
        ),
        Without<LocalPlayerMarker>,
    >,
    mut local_health: Query<&mut Health, With<LocalPlayerMarker>>,
    // A plain tuple of `ResMut`s is itself one `SystemParam` (and one
    // function parameter, destructured right here) -- this system was
    // already at Bevy's own system-param arity ceiling before
    // `light_orb::NetworkLightOrbs` needed a slot too -- merged into a
    // tuple rather than a 17th top-level param.
    (mut network_hitboxes, mut network_light_orbs, mut vision_floors): (
        ResMut<NetworkHitboxes>,
        ResMut<crate::light_orb::NetworkLightOrbs>,
        ResMut<crate::floor_display::VisionFloors>,
    ),
    mut fades: Query<&mut Fade, Without<LocalPlayerMarker>>,
    mut local_vision: Query<&mut VisionRadius, With<LocalPlayerMarker>>,
    mut pending_reconciliation: ResMut<PendingReconciliation>,
    mut pending_revive: ResMut<PendingRevive>,
    asset_server: Res<AssetServer>,
    gameplay_config: Res<GameplayConfig>,
    // Everything that turns a snapshot's `NameId`s into definitions --
    // one tuple, since this system is at Bevy's param-count ceiling.
    (creatures, npcs, names): (Res<CreatureRegistry>, Res<NpcRegistry>, Res<WireNames>),
    mut game_clock: ResMut<GameClock>,
) {
    let mut received_any = false;
    let mut seen: HashSet<NetworkId> = HashSet::new();
    while let Some(bytes) = client.receive_message(DefaultChannel::Unreliable) {
        received_any = true;
        let Ok(ServerMessage::Snapshot {
            entities,
            active_hitboxes,
            light_orbs,
            game_time_hours,
            your_vision_radius,
            your_last_processed_input_tick,
            vision_floors: floors,
            floor_exits,
            ..
        }) = protocol::decode::<ServerMessage>(&bytes)
        else {
            continue;
        };
        // Wholesale overwrite, not merged/appended -- see
        // `NetworkHitboxes`'s own doc for why there's nothing to
        // reconcile here.
        network_hitboxes.0 = active_hitboxes;
        // Same wholesale-overwrite treatment -- see
        // `light_orb::NetworkLightOrbs`'s own doc.
        network_light_orbs.0 = light_orbs;
        if vision_floors.0 != floors {
            vision_floors.0 = floors;
        }
        // Authoritative overwrite, not a correction blended in -- same
        // "server tells the truth" rule as Position, just with nothing
        // to reconcile since GameClock has no local input to predict.
        game_clock.hours = game_time_hours;
        // Same rule for our own VisionRadius -- and it's the only source:
        // the client never recomputes it (game_core's
        // recompute_vision_radius is server-only, see its doc). This is
        // what the vision-mask shader actually reads.
        if let Ok(mut vision) = local_vision.get_single_mut() {
            vision.set_if_neq(VisionRadius(your_vision_radius));
        }
        for snapshot in entities {
            seen.insert(snapshot.id);
            if snapshot.id == local_player.network_id {
                pending_reconciliation.stage(PendingCorrection {
                    server_position: snapshot.position,
                    server_level: snapshot.level,
                    last_processed_input_tick: your_last_processed_input_tick,
                });
                // Position gets the full reconciliation-replay treatment
                // above since it has local input to replay on top of;
                // Health has no local prediction to preserve at all the
                // rest of the time (the local player can never land a
                // hit on itself, and a remote attacker's own Hitbox is
                // never simulated on this client -- see
                // `tick_attacking_state`'s own doc), so there's nothing
                // to reconcile, just an authoritative value to copy
                // straight in. Before this, the local player's own
                // Health was set once at connect and never touched
                // again, which read as a health bar stuck at max forever
                // no matter how much damage the server said actually
                // landed.
                //
                // The one exception: while `PendingRevive` is `Some`, a
                // snapshot that doesn't yet postdate that revive request
                // is skipped entirely -- see that resource's own doc for
                // the "un-revives you" bug this closes.
                let revive_confirmed = pending_revive.0.map_or(true, |sent_at| your_last_processed_input_tick >= sent_at);
                if revive_confirmed {
                    pending_revive.0 = None;
                    if let Ok(mut health) = local_health.get_single_mut() {
                        health.current = snapshot.health;
                        health.max = snapshot.max_health;
                    }
                }
                continue;
            }
            let entity = *remotes.entities.entry(snapshot.id).or_insert_with(|| {
                let (half_extents, texture_path) = match snapshot.kind {
                    EntityKind::Player => (gameplay_config.player_half_extents_vec2(), INITIAL_TEXTURE.to_string()),
                    EntityKind::Creature(id) => {
                        let id = names.name(id);
                        let half_extents = creatures
                            .creatures
                            .get(id)
                            .map(|def| def.half_extents_vec2())
                            .unwrap_or_else(|| gameplay_config.player_half_extents_vec2());
                        (half_extents, format!("animals/{id}/rotations/south.png"))
                    }
                    // No `rotations/<direction>.png` fallback for an NPC
                    // (see `client::animation`'s own NPC-loading doc) --
                    // this placeholder texture is overwritten within the
                    // same or next frame by `animate_npcs` regardless,
                    // the exact same "briefly wrong, instantly corrected"
                    // deal `INITIAL_TEXTURE` already is for a brand new
                    // player before its own real sprite loads.
                    EntityKind::Npc(id) => {
                        let half_extents = npcs
                            .npcs
                            .get(names.name(id))
                            .map(|def| def.half_extents_vec2())
                            .unwrap_or_else(|| gameplay_config.player_half_extents_vec2());
                        (half_extents, INITIAL_TEXTURE.to_string())
                    }
                };
                println!(
                    "[client] new remote {} {:?}",
                    match &snapshot.kind {
                        EntityKind::Player => "player",
                        EntityKind::Creature(_) => "creature",
                        EntityKind::Npc(_) => "npc",
                    },
                    snapshot.id
                );
                // A creature can be dead already the very first time this
                // client ever hears about it -- it died while out of
                // vision, then wandered (or was walked toward) back into
                // range, and `apply_remote_snapshots`'s own vision-based
                // despawn (see that function's doc) means this is a
                // genuinely brand new entity, not the one that was alive
                // moments ago. Starting its AnimationState pre-finished
                // (see `AnimationState::already_dead`'s own doc) instead
                // of the default is what stops that from replaying the
                // whole death animation on a corpse that already
                // finished dying, possibly minutes ago.
                let animation = if snapshot.combat_state == CombatState::Dead {
                    AnimationState::already_dead()
                } else {
                    AnimationState::default()
                };
                let mut entity_commands = commands.spawn((
                    snapshot.id,
                    Position(snapshot.position),
                    // Authoritative, not a guessed default -- a corpse
                    // freshly (re)spawned after leaving and re-entering
                    // vision has zero velocity to derive a direction
                    // from, so without this it always snapped to
                    // `Facing::default()` (South) regardless of which
                    // way it actually died facing.
                    snapshot.facing,
                    // Authoritative too: this entity can't be written to
                    // until the next frame (it's spawned via `commands`),
                    // and a default `Idle` for that one frame reset a
                    // corpse's `AnimationState::already_dead` -- replaying
                    // its whole death every time it came back into view.
                    snapshot.combat_state,
                    animation,
                    // Deliberately NO Velocity here: `Has<Velocity>` is
                    // what resolve_solid_collisions uses to decide
                    // "movable" vs "immovable, treat like a wall". A
                    // remote entity's real position is network truth,
                    // not something local collision math should ever
                    // touch -- without Velocity it still blocks the
                    // local player, but never gets pushed itself, so
                    // it can't fight the next incoming Snapshot.
                    SolidBody { half_extents },
                    // NO Velocity here (see the SolidBody comment
                    // above) -- and for the same reason, Airborne's
                    // height is only ever written from the snapshot
                    // below, never integrated locally by
                    // apply_jump_physics (it's gated on Has<Velocity>).
                    Airborne::default(),
                    // Lets the *local* player's own predicted Hitbox
                    // register a hit against this remote entity
                    // immediately (game_core::systems::combat::
                    // resolve_hitboxes runs client-side too) instead of
                    // only ever reacting once a snapshot round-trip
                    // confirms it -- without a Hurtbox/Health here, a
                    // locally-swung attack against a creature never
                    // visibly connected at all client-side, which read as
                    // "attacks doing nothing". `current` is corrected
                    // from `snapshot.health` below every snapshot
                    // regardless of whatever this predicted locally, so a
                    // misprediction can't linger.
                    Hurtbox { half_extents },
                    Health { current: snapshot.health, max: snapshot.max_health },
                    // See the update-branch comment below for why this is
                    // a plain authoritative copy, same as position/health.
                    Level(snapshot.level),
                    Fade::fade_in(),
                    // Drawn a little behind its latest snapshot, blending
                    // between them -- see crate::interpolation.
                    crate::interpolation::SnapshotHistory::default(),
                    // A player/creature's sprite can extend visually
                    // beyond its own hitbox too -- see crate::YSorted's
                    // own doc. Every remote entity gets this regardless
                    // of player-vs-creature, same as the local player
                    // below.
                    crate::YSorted,
                    SpriteBundle {
                        texture: asset_server.load(texture_path),
                        ..default()
                    },
                ));
                match snapshot.kind {
                    EntityKind::Creature(id) => {
                        entity_commands.insert(Creature(names.name(id).to_owned()));
                    }
                    // Never hittable, not even cosmetically client-side --
                    // see `game_core::npc`'s own module doc. Every other
                    // remote kind keeps the `Hurtbox` the bundle above
                    // just gave it (see that spawn site's own comment for
                    // why); an NPC is the one kind that must not.
                    EntityKind::Npc(id) => {
                        entity_commands.insert(Npc(names.name(id).to_owned())).remove::<Hurtbox>();
                    }
                    // Only a player ever charges a bow or casts an
                    // ability -- see ChargeFraction/CastingAbilityId's
                    // own docs.
                    EntityKind::Player => {
                        entity_commands.insert((
                            Player,
                            crate::charge_display::ChargeFraction::default(),
                            crate::cast_circle_display::CastingAbilityId::default(),
                            crate::aim_display::AimIndicator::default(),
                            Pushing::default(),
                            crate::animation::WeaponTypeIndicator::default(),
                        ));
                    }
                };
                entity_commands.id()
            });
            // Cancels a pending fade-out if this entity was mid-way
            // through leaving (see this function's own doc) -- it just
            // showed up in a snapshot again, so whatever alpha it had
            // reached keeps heading toward fully visible instead of
            // continuing toward despawn. Also resets the hysteresis
            // counter below -- a real, sustained departure needs to
            // start counting misses from zero again, not from wherever
            // a previous brief blip left off.
            if let Ok(mut fade) = fades.get_mut(entity) {
                fade.fading_out = false;
                fade.missing_ticks = 0;
            }
            if let Ok((
                mut position,
                mut facing,
                mut state,
                mut airborne,
                mut health,
                mut level,
                charge_fraction,
                casting_ability,
                aim_indicator,
                (is_pushing, weapon_type),
            )) = remote_state.get_mut(entity)
            {
                position.0 = snapshot.position;
                airborne.height = snapshot.height;
                // Authoritative, no local prediction to preserve -- a
                // remote entity's own floor is exactly as much "network
                // truth, not something local logic should guess" as its
                // Position already is (see the SolidBody comment at the
                // spawn site: no Velocity means no local physics touches
                // this entity at all).
                level.0 = snapshot.level;
                // Corrects whatever the local resolve_hitboxes may have
                // predicted (see the spawn-site comment above) back to
                // the server's real number every snapshot.
                health.current = snapshot.health;
                // Authoritative, not just a spawn-time default (see the
                // spawn site's own doc for the `0/-1` bug this fixes) --
                // kept in sync every tick too in case max_health can ever
                // change mid-fight later (a buff, an equipment swap).
                health.max = snapshot.max_health;
                // Authoritative now, same reasoning as combat_state below:
                // the server already runs this same entity's own
                // Facing::from_velocity every tick (see
                // core::systems::movement::update_facing_and_movement_state),
                // so trusting its answer directly is exactly as accurate
                // as re-deriving it here from the snapshot's velocity, and
                // unlike re-deriving, it also survives this entity being
                // torn down and respawned (see the spawn-site comment
                // above) since it doesn't depend on a local Facing value
                // having already existed to hold its ground while
                // velocity sits at zero.
                *facing = snapshot.facing;
                // Authoritative now, not derived: the server already knows
                // Attacking/Dead/Hitstun, states velocity alone could never
                // distinguish from Idle.
                *state = snapshot.combat_state;
                // Only a player-mirror entity has this (see the spawn-site
                // comment above) -- a creature's snapshot always carries
                // charge_fraction: 0.0 anyway (see broadcast_snapshots),
                // so there'd be nothing to copy even if it did.
                if let Some(mut charge_fraction) = charge_fraction {
                    charge_fraction.fraction = snapshot.charge_fraction;
                    charge_fraction.minimum = snapshot.minimum_charge_fraction;
                }
                // Same "only a player-mirror entity has this" note as
                // charge_fraction above.
                if let Some(mut casting_ability) = casting_ability {
                    let id = snapshot.casting_ability_id.map(|id| names.name(id));
                    if casting_ability.0.as_deref() != id {
                        casting_ability.0 = id.map(str::to_owned);
                    }
                }
                // Same "only a player-mirror entity has this" note again.
                // `combat_state == Charging` alone isn't enough -- that's
                // also true mid-ability-cast (see `AimAngle`'s own doc:
                // it only ever exists for a bow's own draw) -- so this
                // also requires `casting_ability_id` to be `None`, the
                // wire equivalent of "this charge is a bow, not a spell".
                if let Some(mut aim_indicator) = aim_indicator {
                    aim_indicator.angle = snapshot.aim_angle;
                    aim_indicator.visible =
                        snapshot.combat_state == CombatState::Charging && snapshot.casting_ability_id.is_none();
                }
                // Same "only a player-mirror entity has this" note again.
                if let Some(mut is_pushing) = is_pushing {
                    is_pushing.0 = snapshot.pushing;
                }
                if let Some(mut weapon_type) = weapon_type {
                    let name = snapshot.weapon_type.map(|id| names.name(id));
                    if weapon_type.0.as_deref() != name {
                        weapon_type.0 = name.map(str::to_owned);
                    }
                }
            }
        }
        // Fully faded out -- `fade::despawn_finished_fadeouts` removes it.
        for id in floor_exits {
            let Some(&entity) = remotes.entities.get(&id) else { continue };
            if let Ok(mut fade) = fades.get_mut(entity) {
                fade.fading_out = true;
                fade.alpha = 0.0;
            }
        }
    }

    if received_any {
        for (net_id, &entity) in remotes.entities.iter() {
            if seen.contains(net_id) {
                continue;
            }
            if let Ok(mut fade) = fades.get_mut(entity) {
                // Only actually start fading out once this has been
                // missing for several calls in a row -- see
                // MISSING_TICKS_BEFORE_FADE_OUT's own doc for why a
                // single miss isn't trusted on its own (vision-boundary
                // jitter, not necessarily a real departure).
                fade.missing_ticks += 1;
                if fade.missing_ticks >= crate::fade::MISSING_TICKS_BEFORE_FADE_OUT {
                    fade.fading_out = true;
                }
            }
        }
    }
}
