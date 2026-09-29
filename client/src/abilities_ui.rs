//! The floating "Abilities" window -- spending banked `components::
//! ProfessionPoints` (granted by the separate overall `components::
//! CharacterLevel`, shown in `client::character_stats_ui` instead) to
//! advance a profession's own level via `SpendProfessionPoint`, spending
//! a profession's ability picks via `LearnAbility` (a learned ability then
//! ranks up by itself as the profession levels -- see `game_core::
//! profession`'s own module doc), reassigning which of the fixed 6 hotbar slots a known ability occupies
//! via `SwapKnownAbilities`, and choosing each slot's key -- click the key
//! next to an ability, then press any free key (`capture_rebind_key`).
//! The player's keys are saved in their settings folder
//! (`config::keybindings_path`).
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
//! Same "despawn and rebuild the whole window" shape `character_stats_ui`
//! uses, rebuilding only when something it shows actually changes: the
//! local player's classes, known abilities or profession points, the key
//! bindings, or an in-progress rebind.

use bevy::prelude::*;
use bevy_renet::renet::{DefaultChannel, RenetClient};

use game_core::ability::{AbilityDefinition, AbilityId, AbilityRegistry};
use game_core::components::{Classes, KnownAbilities, ProfessionPoints};
use game_core::profession::{ProfessionId, ProfessionRegistry, MAX_ABILITY_LEVEL};
use protocol::ClientMessage;

use crate::config::{key_label, keybindings_path, InputConfig, PlayerAction, ReservedKeys, ABILITY_ACTIONS};
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

/// Whether the Abilities window is currently open -- toggled by
/// `AbilitiesToggleButton`'s own click handler.
#[derive(Resource, Default)]
pub struct AbilitiesWindow {
    pub open: bool,
}

/// Which fixed hotbar slot (0..6), if any, is currently waiting for the
/// player to press a replacement key -- see `capture_rebind_key`. Purely
/// client-local: rebinding a slot edits this client's own
/// `client::config::InputConfig` and saves it to the player's key
/// bindings file, never `config/input.ron` and never the server -- the
/// server has no notion of physical keys at all, only the resulting
/// `AbilitySlotInputs` index. `pub(crate)` field so `client::chat_ui` can
/// check it for its own mutual-exclusion guard -- see
/// `capture_rebind_key`'s own doc.
#[derive(Resource, Default)]
pub(crate) struct RebindingSlot(pub(crate) Option<usize>);

/// What the last key change did ("Fireball is now on G."), or why a key
/// was refused -- shown at the top of the window until the next one, or
/// until it closes.
#[derive(Resource, Default)]
struct RebindNotice(Option<String>);

/// The Equipment panel's own "Abilities" button.
#[derive(Component)]
pub struct AbilitiesToggleButton;

#[derive(Component)]
struct AbilitiesWindowRoot;

#[derive(Component, Clone)]
enum AbilitiesAction {
    Close,
    Learn { profession: ProfessionId, ability: AbilityId },
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
    /// Puts every hotbar slot back on its `config/input.ron` key.
    ResetKeys,
}

pub struct AbilitiesUiPlugin;

impl Plugin for AbilitiesUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AbilitiesWindow>();
        app.init_resource::<RebindingSlot>();
        app.init_resource::<RebindNotice>();
        // Right after the keyboard is read, before anything acts on it --
        // see capture_rebind_key.
        app.add_systems(PreUpdate, capture_rebind_key.after(bevy::input::InputSystem));
        app.add_systems(Update, (handle_toggle_button, sync_window, handle_actions, close_on_cancel));
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

/// What binding a key to a hotbar slot did -- see `bind_slot_key`.
#[derive(Debug, PartialEq)]
enum Rebind {
    /// The slot now uses the key.
    Bound,
    /// The key was `other_slot`'s, which took this slot's old key(s).
    Swapped { other_slot: usize },
    /// The slot already used it.
    Unchanged,
    /// Something else uses the key (named); nothing changed.
    Taken(String),
}

/// Points hotbar slot `slot` at `key` alone. A key another slot had is
/// swapped (that slot takes this one's old key), so one key never fires
/// two abilities. A key that moves, attacks, opens chat and so on is
/// refused rather than taken from it -- see `config::ReservedKeys`.
fn bind_slot_key(config: &mut InputConfig, reserved: &ReservedKeys, slot: usize, key: KeyCode) -> Rebind {
    let action = ABILITY_ACTIONS[slot];
    if let Some(what) = reserved.used_for(key) {
        return Rebind::Taken(what.to_string());
    }
    match config.action_for(key) {
        Some(owner) if owner == action => Rebind::Unchanged,
        Some(owner) => match ABILITY_ACTIONS.iter().position(|a| *a == owner) {
            Some(other_slot) => {
                let old = config.bindings.get(&action).cloned().unwrap_or_default();
                config.bindings.insert(owner, old);
                config.bindings.insert(action, vec![key]);
                Rebind::Swapped { other_slot }
            }
            None => Rebind::Taken(owner.label().to_string()),
        },
        None => {
            config.bindings.insert(action, vec![key]);
            Rebind::Bound
        }
    }
}

/// Saves the player's key changes, noting in `message` if that failed.
fn save_keys(message: String, config: &InputConfig) -> String {
    match keybindings_path() {
        Some(path) => match config.save_player_bindings(&path) {
            Ok(()) => message,
            Err(e) => format!("{message} (couldn't save it: {e})"),
        },
        None => format!("{message} (not saved: no settings folder found)"),
    }
}

/// While `RebindingSlot` names a slot, takes the next key pressed as that
/// slot's new key (`bind_slot_key`) and saves it. Escape cancels instead.
/// A refused key leaves it waiting for another. Runs right after the
/// keyboard is read and consumes the press (`ButtonInput::reset`), so it
/// doesn't also move, attack, cast, open chat or close the window this
/// frame.
#[allow(clippy::too_many_arguments)]
fn capture_rebind_key(
    mut rebinding: ResMut<RebindingSlot>,
    mut input_config: ResMut<InputConfig>,
    mut keyboard: ResMut<ButtonInput<KeyCode>>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    window: Res<AbilitiesWindow>,
    reserved: Res<ReservedKeys>,
    mut notice: ResMut<RebindNotice>,
    abilities: Res<AbilityRegistry>,
    known: Query<&KnownAbilities, With<LocalPlayerMarker>>,
) {
    // Mutual exclusion with chat's own modal key-capture -- see
    // `chat_ui::handle_enter_key`'s own doc for the other half of this.
    // Cancels any in-progress rebind outright rather than letting the two
    // capture modes fight over the next keypress. A closed window can't
    // be waiting for a key either.
    if chat_window.open || !window.open {
        // Only when there's a rebind to cancel -- writing it every frame
        // would rebuild the Abilities window every frame while chat is open.
        if rebinding.0.is_some() {
            rebinding.0 = None;
        }
        return;
    }
    let Some(slot) = rebinding.0 else { return };
    let Some(&key) = keyboard.get_just_pressed().next() else { return };
    keyboard.reset(key);
    if key == KeyCode::Escape {
        rebinding.0 = None;
        notice.0 = None;
        return;
    }

    let hotbar = known.get_single().map(|k| hotbar_order(k, &abilities)).unwrap_or_default();
    let name = |slot: usize| {
        hotbar
            .get(slot)
            .map(|id| abilities.abilities.get(*id).map_or(id.as_str(), AbilityDefinition::display_name).to_string())
            .unwrap_or_else(|| format!("Slot {}", slot + 1))
    };
    let first_key = |config: &InputConfig, slot: usize| {
        config.bindings.get(&ABILITY_ACTIONS[slot]).and_then(|keys| keys.first()).map_or_else(|| "no key".to_string(), |&k| key_label(k))
    };
    let label = key_label(key);
    let message = match bind_slot_key(&mut input_config, &reserved, slot, key) {
        Rebind::Taken(what) => {
            notice.0 = Some(format!("{label} is used for {what} -- press another key, or Esc to cancel."));
            return;
        }
        Rebind::Unchanged => format!("{} is already on {label}.", name(slot)),
        Rebind::Bound => save_keys(format!("{} is now on {label}.", name(slot)), &input_config),
        Rebind::Swapped { other_slot } => save_keys(
            format!("{} is now on {label}; {} moved to {}.", name(slot), name(other_slot), first_key(&input_config, other_slot)),
            &input_config,
        ),
    };
    println!("[abilities] {message}");
    notice.0 = Some(message);
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
    local: Query<(&Classes, Option<&KnownAbilities>, Option<&ProfessionPoints>), With<LocalPlayerMarker>>,
    changed: Query<
        (),
        (
            With<LocalPlayerMarker>,
            Or<(Changed<Classes>, Changed<KnownAbilities>, Changed<ProfessionPoints>)>,
        ),
    >,
    input_config: Res<InputConfig>,
    rebinding: Res<RebindingSlot>,
    notice: Res<RebindNotice>,
) {
    let just_toggled = window.is_changed();
    // A rebind starting/finishing counts too, so the clicked slot shows
    // "Press a key..." and then its new key label immediately.
    let content_changed =
        !changed.is_empty() || input_config.is_changed() || rebinding.is_changed() || notice.is_changed();
    if !just_toggled && !(window.open && content_changed) {
        return;
    }

    for entity in &existing {
        commands.entity(entity).despawn_recursive();
    }
    if !window.open {
        return;
    }
    let Ok((classes, known, profession_points)) = local.get_single() else { return };
    let banked_profession_points = profession_points.map_or(0, |p| p.0);
    let no_abilities = KnownAbilities::default();
    let known = known.unwrap_or(&no_abilities);
    let hotbar = hotbar_order(known, &abilities);

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
                        format!(
                            "Profession Points: {banked_profession_points}   Profession budget: {}/{}",
                            classes.points_used(&professions),
                            professions.budget.total
                        ),
                        TextStyle { font: font.clone(), font_size: 12.0, color: SECTION_COLOR },
                    ));
                    // How to change a key, what the last change did, or
                    // which ability is waiting for its new key.
                    let (hint, hint_color) = match (rebinding.0, &notice.0) {
                        (Some(_), Some(refused)) => (refused.clone(), TITLE_COLOR),
                        (Some(slot), None) => {
                            let name = hotbar
                                .get(slot)
                                .map(|id| abilities.abilities.get(*id).map_or(id.as_str(), AbilityDefinition::display_name))
                                .unwrap_or("this slot");
                            (format!("Press the new key for {name} -- Esc cancels."), TITLE_COLOR)
                        }
                        (None, Some(done)) => (done.clone(), LABEL_COLOR),
                        (None, None) => ("To change a key, click it (like [1]) and press any key.".to_string(), DIM_COLOR),
                    };
                    let keys_changed =
                        ABILITY_ACTIONS.iter().any(|action| input_config.bindings.get(action) != input_config.defaults.get(action));
                    body.spawn(NodeBundle {
                        style: Style {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            justify_content: JustifyContent::SpaceBetween,
                            column_gap: Val::Px(6.0),
                            ..default()
                        },
                        ..default()
                    })
                    .with_children(|row| {
                        let show_reset = keys_changed && rebinding.0.is_none();
                        // Room for the Reset button beside it only when it's there.
                        let hint_width = WINDOW_WIDTH_PX - if show_reset { 110.0 } else { 20.0 };
                        row.spawn(
                            TextBundle::from_section(hint, TextStyle { font: font.clone(), font_size: 11.0, color: hint_color })
                                .with_style(Style { max_width: Val::Px(hint_width), ..default() }),
                        );
                        if show_reset {
                            spawn_small_button(row, &font, "Reset keys", AbilitiesAction::ResetKeys);
                        }
                    });
                    spawn_separator(body);
                    for progress in classes.all() {
                        let Some(def) = professions.professions.get(&progress.profession) else { continue };
                        spawn_section_title(body, &font, &format!("{} ({})", def.display_name, def.category.label()));
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
                                format!("Lv {}/{}", progress.level, def.max_level),
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

                        // Free picks per tier, or when the next ones come.
                        let schedule = professions.ability_picks(&progress.profession);
                        let mut tiers: Vec<u32> =
                            schedule.iter().flat_map(|unlock| unlock.grants.iter().map(|grant| grant.tier)).collect();
                        tiers.sort_unstable();
                        tiers.dedup();
                        let free_picks: Vec<(u32, usize)> = tiers
                            .iter()
                            .map(|&tier| (tier, professions.tier_picks(&abilities, known, progress, tier).free()))
                            .filter(|&(_, free)| free > 0)
                            .collect();
                        let next_unlock =
                            schedule.iter().map(|unlock| unlock.level).filter(|&level| level > progress.level).min();
                        let (picks_line, picks_color) = if !free_picks.is_empty() {
                            let listed: Vec<String> =
                                free_picks.iter().map(|(tier, free)| format!("{free} x Tier {tier}")).collect();
                            (format!("Picks to spend: {}", listed.join(", ")), VALUE_COLOR)
                        } else if let Some(level) = next_unlock {
                            (format!("Next picks at Lv {level}"), DIM_COLOR)
                        } else {
                            (String::new(), DIM_COLOR)
                        };
                        if !picks_line.is_empty() {
                            body.spawn(TextBundle::from_section(
                                picks_line,
                                TextStyle { font: font.clone(), font_size: 11.0, color: picks_color },
                            ));
                        }

                        for ability_id in &def.available_abilities {
                            let ability_def = abilities.abilities.get(ability_id);
                            let display_name = ability_def.map(AbilityDefinition::display_name).unwrap_or(ability_id.as_str());
                            let tier = ability_def.map_or(0, AbilityDefinition::tier);
                            let known_slot =
                                known.0.iter().find(|s| s.profession == progress.profession && &s.ability == ability_id);
                            let known_elsewhere = known.0.iter().any(|s| &s.ability == ability_id);
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
                                            format!("T{tier} {display_name} (Rank {}/{MAX_ABILITY_LEVEL})", slot.level),
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
                                                "press a key".to_string()
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
                                    }
                                    None => {
                                        let label = if known_elsewhere {
                                            format!("T{tier} {display_name} (known)")
                                        } else {
                                            format!("T{tier} {display_name}")
                                        };
                                        row.spawn(TextBundle::from_section(
                                            label,
                                            TextStyle { font: font.clone(), font_size: 11.0, color: DIM_COLOR },
                                        ));
                                        let pick_free = free_picks.iter().any(|&(pick_tier, _)| pick_tier == tier);
                                        if pick_free && !known_elsewhere {
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
    mut notice: ResMut<RebindNotice>,
    mut input_config: ResMut<InputConfig>,
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
                rebinding.0 = None;
                notice.0 = None;
                None
            }
            AbilitiesAction::Learn { profession, ability } => {
                Some(ClientMessage::LearnAbility { profession: profession.clone(), ability: ability.clone() })
            }
            AbilitiesAction::SwapWithNeighbor { ability, neighbor } => {
                Some(ClientMessage::SwapKnownAbilities { ability_a: ability.clone(), ability_b: neighbor.clone() })
            }
            AbilitiesAction::SpendPoint { profession } => {
                Some(ClientMessage::SpendProfessionPoint { profession: profession.clone() })
            }
            AbilitiesAction::StartRebind { slot_index } => {
                rebinding.0 = Some(*slot_index);
                notice.0 = None;
                None
            }
            AbilitiesAction::ResetKeys => {
                for action in ABILITY_ACTIONS {
                    if let Some(keys) = input_config.defaults.get(&action).cloned() {
                        input_config.bindings.insert(action, keys);
                    }
                }
                notice.0 = Some(save_keys("Ability keys are back to the defaults.".to_string(), &input_config));
                None
            }
        };
        if let Some(message) = message {
            if let Ok(bytes) = protocol::encode(&message) {
                client.send_message(DefaultChannel::ReliableOrdered, bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ReserveKey;

    fn config() -> InputConfig {
        let mut config: InputConfig = "(bindings: { MoveUp: [KeyW], Interact: [KeyE], Ability1: [Digit1], Ability2: [Digit2] })"
            .parse()
            .unwrap();
        config.apply_player_bindings(None);
        config
    }

    fn reserved() -> ReservedKeys {
        let mut app = App::new();
        app.reserve_key(KeyCode::Enter, "opening chat");
        app.world.remove_resource::<ReservedKeys>().unwrap()
    }

    #[test]
    fn a_free_key_is_bound() {
        let mut config = config();
        assert_eq!(bind_slot_key(&mut config, &reserved(), 0, KeyCode::KeyG), Rebind::Bound);
        assert_eq!(config.bindings[&PlayerAction::Ability1], vec![KeyCode::KeyG]);
    }

    #[test]
    fn another_abilitys_key_swaps_the_two() {
        let mut config = config();
        assert_eq!(bind_slot_key(&mut config, &reserved(), 0, KeyCode::Digit2), Rebind::Swapped { other_slot: 1 });
        assert_eq!(config.bindings[&PlayerAction::Ability1], vec![KeyCode::Digit2]);
        assert_eq!(config.bindings[&PlayerAction::Ability2], vec![KeyCode::Digit1]);
    }

    #[test]
    fn controls_and_reserved_keys_are_refused() {
        let mut config = config();
        assert_eq!(bind_slot_key(&mut config, &reserved(), 0, KeyCode::KeyW), Rebind::Taken("Move Up".to_string()));
        assert_eq!(bind_slot_key(&mut config, &reserved(), 0, KeyCode::Enter), Rebind::Taken("opening chat".to_string()));
        assert_eq!(config.bindings[&PlayerAction::Ability1], vec![KeyCode::Digit1], "nothing changed");
        assert_eq!(bind_slot_key(&mut config, &reserved(), 0, KeyCode::Digit1), Rebind::Unchanged);
    }

    /// The press that picks the key is the rebind's alone: gameplay reading
    /// the keyboard later that frame doesn't see it, and Escape cancels
    /// without also closing the window.
    #[test]
    fn the_captured_press_is_consumed() {
        use bevy::ecs::system::RunSystemOnce;
        let keys_file = std::env::temp_dir().join(format!("arpg_keybinds_capture_{}.ron", std::process::id()));
        std::env::set_var("ARPG_KEYBINDS_PATH", &keys_file);
        let mut app = App::new();
        app.insert_resource(config());
        app.insert_resource(reserved());
        app.insert_resource(ButtonInput::<KeyCode>::default());
        app.insert_resource(crate::chat_ui::ChatWindow::default());
        app.insert_resource(AbilitiesWindow { open: true });
        app.insert_resource(RebindingSlot(Some(0)));
        app.insert_resource(RebindNotice::default());
        app.insert_resource(AbilityRegistry::default());

        app.world.resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyG);
        app.world.run_system_once(capture_rebind_key);
        assert!(!app.world.resource::<ButtonInput<KeyCode>>().just_pressed(KeyCode::KeyG), "consumed");
        assert_eq!(app.world.resource::<InputConfig>().bindings[&PlayerAction::Ability1], vec![KeyCode::KeyG]);
        assert_eq!(app.world.resource::<RebindingSlot>().0, None);
        assert_eq!(app.world.resource::<RebindNotice>().0.as_deref(), Some("Slot 1 is now on G."));

        app.world.resource_mut::<RebindingSlot>().0 = Some(0);
        app.world.resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::Escape);
        app.world.run_system_once(capture_rebind_key);
        assert!(!app.world.resource::<ButtonInput<KeyCode>>().just_pressed(KeyCode::Escape), "Esc doesn't also close the window");
        assert_eq!(app.world.resource::<RebindingSlot>().0, None);
        assert_eq!(app.world.resource::<InputConfig>().bindings[&PlayerAction::Ability1], vec![KeyCode::KeyG], "unchanged");
        let _ = std::fs::remove_file(&keys_file);
    }
}
