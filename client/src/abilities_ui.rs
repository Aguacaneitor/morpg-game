//! The floating "Abilities" window -- spending banked `components::
//! ProfessionPoints` (granted by the separate overall `components::
//! CharacterLevel`, shown in `client::character_stats_ui` instead) to
//! advance a profession's own level via `SpendProfessionPoint`, learning/
//! leveling known spells and skills with the profession's own banked
//! `components::SpellPoints` via `LearnAbility`/`LevelUpAbility`, and
//! reassigning which of the fixed 6 hotbar keys a known ability occupies
//! via `SwapKnownAbilities`.
//! Opened via the Equipment panel's own "Abilities" button (`client::ui::
//! spawn_equipment_body`) -- split out from `client::character_stats_ui`'s
//! own window (which stays a quick Attributes/Stats readout) so each
//! window has one clear job.
//!
//! Covers both `AbilityCategory::Skill` and `Magic` in one list rather
//! than two separate windows -- most professions lean heavily one way or
//! the other already (see each `data/professions.ron` entry's own
//! `available_abilities`), so a category split would mean one tab
//! sitting empty for most characters.
//!
//! Same "despawn and rebuild on a refresh timer" shape `character_stats_ui`
//! uses -- see that module's own doc for why a plain `Changed<...>` gate
//! doesn't work for `EffectiveStats`-adjacent data.

use bevy::prelude::*;
use bevy_renet::renet::{DefaultChannel, RenetClient};

use game_core::ability::{AbilityDefinition, AbilityId, AbilityRegistry};
use game_core::components::{Classes, KnownAbilities, ProfessionPoints, SpellPoints};
use game_core::profession::{ProfessionId, ProfessionRegistry};
use protocol::ClientMessage;

use crate::config::{key_label, InputConfig, PlayerAction, ABILITY_ACTIONS};
use crate::net::LocalPlayerMarker;

const WINDOW_BG: Color = Color::rgb(0.10, 0.09, 0.08);
const WINDOW_BORDER: Color = Color::rgb(0.42, 0.34, 0.20);
const HEADER_BG: Color = Color::rgb(0.20, 0.16, 0.10);
const TITLE_COLOR: Color = Color::rgb(0.85, 0.78, 0.60);
const SECTION_COLOR: Color = Color::rgb(0.75, 0.65, 0.45);
const LABEL_COLOR: Color = Color::rgb(0.75, 0.75, 0.75);
const VALUE_COLOR: Color = Color::rgb(0.9, 0.9, 0.9);
const DIM_COLOR: Color = Color::rgb(0.5, 0.5, 0.5);
const BUTTON_BG: Color = Color::rgb(0.20, 0.16, 0.10);
const BUTTON_BG_HOVERED: Color = Color::rgb(0.30, 0.24, 0.15);
const BUTTON_BG_PRESSED: Color = Color::rgb(0.42, 0.34, 0.20);
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";
/// Small enough to sit inline with an 11px name label -- see
/// `spawn_ability_icon`'s own doc.
const ICON_SIZE: f32 = 16.0;
const WINDOW_LEFT_PX: f32 = 660.0;
const WINDOW_TOP_PX: f32 = 40.0;
const WINDOW_WIDTH_PX: f32 = 380.0;
const REFRESH_INTERVAL_SECS: f32 = 0.25;

/// Whether the Abilities window is currently open -- toggled by
/// `AbilitiesToggleButton`'s own click handler.
#[derive(Resource, Default)]
pub struct AbilitiesWindow {
    pub open: bool,
}

/// Which fixed hotbar slot (0..6), if any, is currently waiting for the
/// player to press a replacement key -- see `capture_rebind_key`. Purely
/// client-local: rebinding a slot only ever edits this client's own
/// `client::config::InputConfig` in memory (not persisted to `config/
/// input.ron`, not sent to the server -- the server has no notion of
/// physical keys at all, only the resulting `AbilitySlotInputs` index).
/// `pub(crate)` field so `client::chat_ui` can check it for its own
/// mutual-exclusion guard -- see `capture_rebind_key`'s own doc.
#[derive(Resource, Default)]
pub(crate) struct RebindingSlot(pub(crate) Option<usize>);

/// The Equipment panel's own "Abilities" button.
#[derive(Component)]
pub struct AbilitiesToggleButton;

#[derive(Component)]
struct AbilitiesWindowRoot;

#[derive(Component, Clone)]
enum AbilitiesAction {
    Close,
    Learn { profession: ProfessionId, ability: AbilityId },
    LevelUp { profession: ProfessionId, ability: AbilityId },
    /// Swaps this ability's own hotbar position with the known ability
    /// currently occupying the adjacent one -- see `swap_hotbar_neighbor`.
    SwapWithNeighbor { ability: AbilityId, neighbor: AbilityId },
    /// Spends one banked `components::ProfessionPoints` point advancing
    /// this profession's own level by 1 -- see `protocol::ClientMessage::
    /// SpendProfessionPoint`'s own doc.
    SpendPoint { profession: ProfessionId },
    /// Begins (or re-begins) waiting for a key press to rebind this
    /// hotbar slot's own physical key -- see `RebindingSlot`/
    /// `capture_rebind_key`.
    StartRebind { slot_index: usize },
}

pub struct AbilitiesUiPlugin;

impl Plugin for AbilitiesUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AbilitiesWindow>();
        app.init_resource::<RebindingSlot>();
        app.add_systems(
            Update,
            (handle_toggle_button, sync_window, handle_actions, capture_rebind_key, close_on_cancel),
        );
    }
}

/// Same "Escape backs out of the current UI" role `client::interact::
/// close_container_on_cancel` already gives the loot window -- see that
/// system's own doc. Deliberately skipped while `RebindingSlot` is
/// active: `capture_rebind_key` already treats that Escape press as
/// "cancel the rebind" instead, and closing the whole window on top of
/// that too would skip a step the player probably didn't intend.
/// `pub(crate)` so `client::chat_ui::handle_escape_key` can order itself
/// after this -- see that system's own doc for why that ordering (not
/// just a `ChatWindow` guard here) is what keeps chat's own Escape-close
/// from also closing this window on the same press.
pub(crate) fn close_on_cancel(
    keyboard: Res<ButtonInput<KeyCode>>,
    input_config: Res<InputConfig>,
    rebinding: Res<RebindingSlot>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    mut window: ResMut<AbilitiesWindow>,
) {
    if !chat_window.open && window.open && rebinding.0.is_none() && input_config.action_just_pressed(&keyboard, PlayerAction::Cancel) {
        window.open = false;
    }
}

/// While `RebindingSlot` names a slot, consumes the next key pressed
/// (ignoring `Escape`, which cancels the rebind instead of becoming its
/// new key -- otherwise a player trying to back out of rebinding would
/// accidentally bind the slot to Escape) and points that slot's own
/// `PlayerAction::AbilityN` at it in `InputConfig`, replacing whatever
/// key(s) it used before. Session-only -- see `RebindingSlot`'s own doc.
fn capture_rebind_key(
    mut rebinding: ResMut<RebindingSlot>,
    mut input_config: ResMut<InputConfig>,
    keyboard: Res<ButtonInput<KeyCode>>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
) {
    // Mutual exclusion with chat's own modal key-capture -- see
    // `chat_ui::handle_enter_key`'s own doc for the other half of this.
    // Cancels any in-progress rebind outright rather than letting the two
    // capture modes fight over the next keypress.
    if chat_window.open {
        rebinding.0 = None;
        return;
    }
    let Some(slot_index) = rebinding.0 else { return };
    if keyboard.just_pressed(KeyCode::Escape) {
        rebinding.0 = None;
        return;
    }
    let Some(&key) = keyboard.get_just_pressed().next() else { return };
    let action = ABILITY_ACTIONS[slot_index];
    input_config.bindings.insert(action, vec![key]);
    println!("[abilities] slot {} rebound to {}", slot_index + 1, key_label(key));
    rebinding.0 = None;
}

fn handle_toggle_button(
    mut window: ResMut<AbilitiesWindow>,
    mut buttons: Query<(&Interaction, &mut BackgroundColor), (With<AbilitiesToggleButton>, Changed<Interaction>)>,
) {
    for (interaction, mut background) in &mut buttons {
        *background = match interaction {
            Interaction::Pressed => BUTTON_BG_PRESSED,
            Interaction::Hovered => BUTTON_BG_HOVERED,
            Interaction::None => BUTTON_BG,
        }
        .into();
        if *interaction == Interaction::Pressed {
            window.open = !window.open;
        }
    }
}

/// The fixed 6-key hotbar, in order -- exactly the same "filter out
/// Passives, index what's left" rule `core::systems::combat::
/// trigger_abilities` itself uses, so the slot numbers shown here always
/// match what pressing that key actually casts.
fn hotbar_order<'a>(known: &'a KnownAbilities, abilities: &AbilityRegistry) -> Vec<&'a AbilityId> {
    known
        .0
        .iter()
        .filter(|slot| !matches!(abilities.abilities.get(&slot.ability), Some(AbilityDefinition::Passive(_))))
        .map(|slot| &slot.ability)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn sync_window(
    mut commands: Commands,
    window: Res<AbilitiesWindow>,
    existing: Query<Entity, With<AbilitiesWindowRoot>>,
    asset_server: Res<AssetServer>,
    professions: Res<ProfessionRegistry>,
    abilities: Res<AbilityRegistry>,
    local: Query<(&Classes, Option<&KnownAbilities>, Option<&SpellPoints>, Option<&ProfessionPoints>), With<LocalPlayerMarker>>,
    input_config: Res<InputConfig>,
    rebinding: Res<RebindingSlot>,
    mut timer: Local<Option<Timer>>,
    time: Res<Time>,
) {
    let just_toggled = window.is_changed();
    let due = timer.as_mut().is_some_and(|t| {
        t.tick(time.delta());
        t.just_finished()
    });
    // Also rebuild the instant a rebind starts/completes, so the clicked
    // slot shows "Press a key..." immediately and its new key label
    // appears immediately too, instead of waiting up to
    // REFRESH_INTERVAL_SECS like every other refresh here does.
    if !just_toggled && !rebinding.is_changed() && !(window.open && due) {
        return;
    }
    if window.open && timer.is_none() {
        *timer = Some(Timer::from_seconds(REFRESH_INTERVAL_SECS, TimerMode::Repeating));
    }
    if !window.open {
        *timer = None;
    }

    for entity in &existing {
        commands.entity(entity).despawn_recursive();
    }
    if !window.open {
        return;
    }
    let Ok((classes, known, spell_points, profession_points)) = local.get_single() else { return };
    let banked_profession_points = profession_points.map_or(0, |p| p.0);
    let hotbar = known.map(|k| hotbar_order(k, &abilities)).unwrap_or_default();

    let font: Handle<Font> = asset_server.load(UI_FONT);

    commands
        .spawn((
            AbilitiesWindowRoot,
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    left: Val::Px(WINDOW_LEFT_PX),
                    top: Val::Px(WINDOW_TOP_PX),
                    width: Val::Px(WINDOW_WIDTH_PX),
                    max_height: Val::Px(560.0),
                    flex_direction: FlexDirection::Column,
                    border: UiRect::all(Val::Px(1.0)),
                    overflow: Overflow::clip_y(),
                    ..default()
                },
                background_color: WINDOW_BG.into(),
                border_color: WINDOW_BORDER.into(),
                z_index: ZIndex::Global(50),
                ..default()
            },
        ))
        .with_children(|window_root| {
            window_root
                .spawn(NodeBundle {
                    style: Style {
                        flex_direction: FlexDirection::Row,
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        padding: UiRect::axes(Val::Px(6.0), Val::Px(4.0)),
                        ..default()
                    },
                    background_color: HEADER_BG.into(),
                    ..default()
                })
                .with_children(|header| {
                    header.spawn(TextBundle::from_section(
                        "Abilities",
                        TextStyle { font: font.clone(), font_size: 14.0, color: TITLE_COLOR },
                    ));
                    spawn_small_button(header, &font, "X", AbilitiesAction::Close);
                });

            window_root
                .spawn(NodeBundle {
                    style: Style {
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(Val::Px(8.0)),
                        row_gap: Val::Px(4.0),
                        overflow: Overflow::clip_y(),
                        ..default()
                    },
                    ..default()
                })
                .with_children(|body| {
                    body.spawn(TextBundle::from_section(
                        format!("Profession Points: {banked_profession_points}"),
                        TextStyle { font: font.clone(), font_size: 12.0, color: SECTION_COLOR },
                    ));
                    spawn_separator(body);
                    for progress in classes.all() {
                        let is_main = progress.profession == classes.main.profession;
                        let Some(def) = professions.professions.get(&progress.profession) else { continue };
                        let banked = spell_points.and_then(|p| p.0.get(&progress.profession)).copied().unwrap_or(0);

                        spawn_section_title(
                            body,
                            &font,
                            &format!("{} {}", def.display_name, if is_main { "(Main)" } else { "(Secondary)" }),
                        );
                        body.spawn(NodeBundle {
                            style: Style {
                                flex_direction: FlexDirection::Row,
                                align_items: AlignItems::Center,
                                column_gap: Val::Px(6.0),
                                ..default()
                            },
                            ..default()
                        })
                        .with_children(|row| {
                            row.spawn(TextBundle::from_section(
                                format!("Lv {}/{}   Spell Points: {}", progress.level, def.max_level, banked),
                                TextStyle { font: font.clone(), font_size: 11.0, color: LABEL_COLOR },
                            ));
                            if banked_profession_points > 0 && progress.level < def.max_level {
                                spawn_small_button(
                                    row,
                                    &font,
                                    "Level Up",
                                    AbilitiesAction::SpendPoint { profession: progress.profession.clone() },
                                );
                            }
                        });

                        let known_count = known
                            .map(|k| k.0.iter().filter(|slot| slot.profession == progress.profession).count() as u32)
                            .unwrap_or(0);

                        for ability_id in &def.available_abilities {
                            let ability_def = abilities.abilities.get(ability_id);
                            let display_name = ability_def.map(AbilityDefinition::display_name).unwrap_or(ability_id.as_str());
                            let known_slot =
                                known.and_then(|k| k.0.iter().find(|s| s.profession == progress.profession && &s.ability == ability_id));
                            let is_passive = matches!(ability_def, Some(AbilityDefinition::Passive(_)));
                            let hotbar_index = hotbar.iter().position(|id| *id == ability_id);

                            body.spawn(NodeBundle {
                                style: Style {
                                    flex_direction: FlexDirection::Row,
                                    align_items: AlignItems::Center,
                                    column_gap: Val::Px(6.0),
                                    padding: UiRect::left(Val::Px(10.0)),
                                    ..default()
                                },
                                ..default()
                            })
                            .with_children(|row| {
                                if let Some(ability_def) = ability_def {
                                    spawn_ability_icon(row, &font, &asset_server, ability_def, ability_id);
                                }
                                match known_slot {
                                    Some(slot) => {
                                        row.spawn(TextBundle::from_section(
                                            format!("{display_name} (Lv {})", slot.level),
                                            TextStyle { font: font.clone(), font_size: 11.0, color: LABEL_COLOR },
                                        ));
                                        if let Some(index) = hotbar_index {
                                            // Passives never reach here (see is_passive's
                                            // own check below) -- only a real hotkeyed
                                            // ability ever has a slot number at all.
                                            // Clickable, not plain text -- shows the
                                            // *actual* physical key this slot fires on
                                            // (e.g. "Q", not a synthetic "5" -- slots 5/6
                                            // are bound to Q/R by default, see config/
                                            // input.ron), and clicking it lets the player
                                            // rebind that key to any letter/number/etc,
                                            // not just the fixed number row.
                                            let label = if rebinding.0 == Some(index) {
                                                "...".to_string()
                                            } else {
                                                input_config
                                                    .bindings
                                                    .get(&ABILITY_ACTIONS[index])
                                                    .and_then(|keys| keys.first())
                                                    .map_or_else(|| "?".to_string(), |&k| key_label(k))
                                            };
                                            spawn_small_button(
                                                row,
                                                &font,
                                                &format!("[{label}]"),
                                                AbilitiesAction::StartRebind { slot_index: index },
                                            );
                                            if index > 0 {
                                                spawn_small_button(
                                                    row,
                                                    &font,
                                                    "^",
                                                    AbilitiesAction::SwapWithNeighbor {
                                                        ability: ability_id.clone(),
                                                        neighbor: hotbar[index - 1].clone(),
                                                    },
                                                );
                                            }
                                            if index + 1 < hotbar.len() {
                                                spawn_small_button(
                                                    row,
                                                    &font,
                                                    "v",
                                                    AbilitiesAction::SwapWithNeighbor {
                                                        ability: ability_id.clone(),
                                                        neighbor: hotbar[index + 1].clone(),
                                                    },
                                                );
                                            }
                                        } else if is_passive {
                                            row.spawn(TextBundle::from_section(
                                                "(passive)",
                                                TextStyle { font: font.clone(), font_size: 11.0, color: DIM_COLOR },
                                            ));
                                        }
                                        if slot.level < game_core::profession::MAX_ABILITY_LEVEL && banked > 0 {
                                            spawn_small_button(
                                                row,
                                                &font,
                                                "Level Up",
                                                AbilitiesAction::LevelUp {
                                                    profession: progress.profession.clone(),
                                                    ability: ability_id.clone(),
                                                },
                                            );
                                        }
                                    }
                                    None => {
                                        row.spawn(TextBundle::from_section(
                                            display_name,
                                            TextStyle { font: font.clone(), font_size: 11.0, color: DIM_COLOR },
                                        ));
                                        if banked > 0 && known_count < def.max_known_abilities {
                                            spawn_small_button(
                                                row,
                                                &font,
                                                "Learn",
                                                AbilitiesAction::Learn {
                                                    profession: progress.profession.clone(),
                                                    ability: ability_id.clone(),
                                                },
                                            );
                                        }
                                    }
                                }
                            });
                        }
                        spawn_separator(body);
                    }
                });
        });
}

/// Resolves `ability_id`'s icon to a `gallery/`-relative path, but only if
/// a file actually exists there -- same "look before you load" rule
/// `item_ui::resolve_icon_path` already uses for items, right down to the
/// `<id>.png` convention when `AbilityDefinition::icon` is left empty.
/// `None` means "use the text-initials fallback instead".
fn resolve_icon_path(def: &AbilityDefinition, ability_id: &str) -> Option<String> {
    let icon = def.icon();
    let relative = if icon.is_empty() { format!("abilities/{ability_id}.png") } else { icon.to_string() };
    std::fs::metadata(format!("gallery/{relative}")).ok()?;
    Some(relative)
}

/// ALL-CAPS initials of the ability's display name -- same convention
/// `item_ui::item_initials`/`ui::EquipmentSlotKind::label` already use,
/// shown in place of a real icon for anything without art yet.
fn ability_initials(display_name: &str) -> String {
    display_name.split_whitespace().filter_map(|word| word.chars().next()).collect::<String>().to_uppercase()
}

/// A small icon-or-initials square, spawned before an ability's own name
/// in every row -- ready for real generated spell/skill art to just drop
/// into `gallery/abilities/<id>.png` with no further wiring.
fn spawn_ability_icon(parent: &mut ChildBuilder, font: &Handle<Font>, asset_server: &AssetServer, def: &AbilityDefinition, ability_id: &str) {
    let mut icon_entity = parent.spawn(NodeBundle {
        style: Style {
            width: Val::Px(ICON_SIZE),
            height: Val::Px(ICON_SIZE),
            flex_shrink: 0.0,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        background_color: BUTTON_BG.into(),
        ..default()
    });
    match resolve_icon_path(def, ability_id) {
        Some(icon_path) => {
            icon_entity.with_children(|icon| {
                icon.spawn(ImageBundle {
                    style: Style { width: Val::Px(ICON_SIZE), height: Val::Px(ICON_SIZE), ..default() },
                    image: UiImage::new(asset_server.load(icon_path)),
                    ..default()
                });
            });
        }
        None => {
            icon_entity.with_children(|icon| {
                icon.spawn(TextBundle::from_section(
                    ability_initials(def.display_name()),
                    TextStyle { font: font.clone(), font_size: 7.0, color: DIM_COLOR },
                ));
            });
        }
    }
}

fn spawn_section_title(parent: &mut ChildBuilder, font: &Handle<Font>, title: &str) {
    parent.spawn(TextBundle::from_section(
        title,
        TextStyle { font: font.clone(), font_size: 13.0, color: SECTION_COLOR },
    ));
}

fn spawn_separator(parent: &mut ChildBuilder) {
    parent.spawn(NodeBundle {
        style: Style {
            width: Val::Percent(100.0),
            height: Val::Px(1.0),
            margin: UiRect::vertical(Val::Px(6.0)),
            ..default()
        },
        background_color: WINDOW_BORDER.into(),
        ..default()
    });
}

fn spawn_small_button(parent: &mut ChildBuilder, font: &Handle<Font>, label: &str, action: AbilitiesAction) {
    parent
        .spawn((
            action,
            NodeBundle {
                style: Style { padding: UiRect::axes(Val::Px(6.0), Val::Px(2.0)), ..default() },
                background_color: BUTTON_BG.into(),
                ..default()
            },
            Interaction::default(),
        ))
        .with_children(|b| {
            b.spawn(TextBundle::from_section(label, TextStyle { font: font.clone(), font_size: 11.0, color: VALUE_COLOR }));
        });
}

fn handle_actions(
    mut window: ResMut<AbilitiesWindow>,
    mut rebinding: ResMut<RebindingSlot>,
    mut client: ResMut<RenetClient>,
    mut buttons: Query<(&Interaction, &AbilitiesAction, &mut BackgroundColor), Changed<Interaction>>,
) {
    for (interaction, action, mut background) in &mut buttons {
        *background = match interaction {
            Interaction::Pressed => BUTTON_BG_PRESSED,
            Interaction::Hovered => BUTTON_BG_HOVERED,
            Interaction::None => BUTTON_BG,
        }
        .into();
        if *interaction != Interaction::Pressed {
            continue;
        }
        let message = match action {
            AbilitiesAction::Close => {
                window.open = false;
                None
            }
            AbilitiesAction::Learn { profession, ability } => {
                Some(ClientMessage::LearnAbility { profession: profession.clone(), ability: ability.clone() })
            }
            AbilitiesAction::LevelUp { profession, ability } => {
                Some(ClientMessage::LevelUpAbility { profession: profession.clone(), ability: ability.clone() })
            }
            AbilitiesAction::SwapWithNeighbor { ability, neighbor } => {
                Some(ClientMessage::SwapKnownAbilities { ability_a: ability.clone(), ability_b: neighbor.clone() })
            }
            AbilitiesAction::SpendPoint { profession } => {
                Some(ClientMessage::SpendProfessionPoint { profession: profession.clone() })
            }
            AbilitiesAction::StartRebind { slot_index } => {
                rebinding.0 = Some(*slot_index);
                None
            }
        };
        if let Some(message) = message {
            if let Ok(bytes) = bincode::serialize(&message) {
                client.send_message(DefaultChannel::ReliableOrdered, bytes);
            }
        }
    }
}
