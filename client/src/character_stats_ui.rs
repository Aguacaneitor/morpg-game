//! The floating "Character" window -- overall `components::CharacterLevel`
//! at the top, then attributes and derived stats, each broken into Base
//! (natural: race/creature + completed profession passive-block growth) /
//! Extra (equipment) / Total. Opened via the Equipment panel's own
//! "Stats" button (`client::ui::spawn_equipment_body`).
//! Profession leveling and known-spell management live in their own
//! separate "Abilities" window (`client::abilities_ui`) -- kept apart so
//! this one stays a quick, focused stat readout.
//!
//! Mirrors `client::loot_ui`'s own "despawn and rebuild the whole window"
//! shape, just gated on a short refresh timer instead of "whenever a
//! server message changes it" -- `components::EffectiveStats` recomputes
//! unconditionally every tick (see that component's own doc), so
//! `Changed<EffectiveStats>` would rebuild this every single tick the
//! window is open regardless of whether anything actually changed.

use bevy::prelude::*;

use game_core::components::{CharacterLevel, EffectiveStats};
use game_core::profession::xp_required_for_level;

use crate::config::{InputConfig, PlayerAction};
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
const WINDOW_LEFT_PX: f32 = 260.0;
const WINDOW_TOP_PX: f32 = 40.0;
const WINDOW_WIDTH_PX: f32 = 380.0;
/// How often the window's contents refresh while open -- see this
/// module's own doc for why this can't just be `Changed<EffectiveStats>`.
const REFRESH_INTERVAL_SECS: f32 = 0.25;

/// Whether the Character Stats window is currently open -- toggled by
/// `StatsToggleButton`'s own click handler.
#[derive(Resource, Default)]
pub struct CharacterStatsWindow {
    pub open: bool,
}

/// The Equipment panel's own "Stats" button -- lives outside the window
/// itself (see `client::ui::spawn_equipment_body`) so it survives the
/// window's own despawn/rebuild cycle.
#[derive(Component)]
pub struct StatsToggleButton;

/// Marks the whole floating window, so it can be despawned wholesale
/// every refresh -- same role `loot_ui::ContainerWindow` plays.
#[derive(Component)]
struct StatsWindowRoot;

#[derive(Component, Clone)]
struct CloseButton;

pub struct CharacterStatsUiPlugin;

impl Plugin for CharacterStatsUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CharacterStatsWindow>();
        app.add_systems(Update, (handle_toggle_button, sync_window, handle_close_button, close_on_cancel));
    }
}

fn handle_toggle_button(
    mut window: ResMut<CharacterStatsWindow>,
    mut buttons: Query<(&Interaction, &mut BackgroundColor), (With<StatsToggleButton>, Changed<Interaction>)>,
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

/// Despawns and rebuilds the whole window every `REFRESH_INTERVAL_SECS`
/// while open (and immediately the instant it opens/closes) -- see this
/// module's own doc for why a plain `Changed<EffectiveStats>` gate can't
/// work here.
fn sync_window(
    mut commands: Commands,
    window: Res<CharacterStatsWindow>,
    existing: Query<Entity, With<StatsWindowRoot>>,
    asset_server: Res<AssetServer>,
    local: Query<(&EffectiveStats, Option<&CharacterLevel>), With<LocalPlayerMarker>>,
    mut timer: Local<Option<Timer>>,
    time: Res<Time>,
) {
    let just_toggled = window.is_changed();
    let due = timer.as_mut().is_some_and(|t| {
        t.tick(time.delta());
        t.just_finished()
    });
    if !just_toggled && !(window.open && due) {
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
    let Ok((stats, character_level)) = local.get_single() else { return };

    let font: Handle<Font> = asset_server.load(UI_FONT);

    commands
        .spawn((
            StatsWindowRoot,
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
                        "Character",
                        TextStyle { font: font.clone(), font_size: 14.0, color: TITLE_COLOR },
                    ));
                    header
                        .spawn((
                            CloseButton,
                            NodeBundle {
                                style: Style { padding: UiRect::axes(Val::Px(8.0), Val::Px(2.0)), ..default() },
                                background_color: BUTTON_BG.into(),
                                ..default()
                            },
                            Interaction::default(),
                        ))
                        .with_children(|b| {
                            b.spawn(TextBundle::from_section(
                                "X",
                                TextStyle { font: font.clone(), font_size: 12.0, color: VALUE_COLOR },
                            ));
                        });
                });

            window_root
                .spawn(NodeBundle {
                    style: Style {
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(Val::Px(8.0)),
                        row_gap: Val::Px(2.0),
                        overflow: Overflow::clip_y(),
                        ..default()
                    },
                    ..default()
                })
                .with_children(|body| {
                    if let Some(level) = character_level {
                        let xp_needed = xp_required_for_level(level.level);
                        body.spawn(TextBundle::from_section(
                            format!("Character Level {}   XP {}/{}", level.level, level.xp, xp_needed),
                            TextStyle { font: font.clone(), font_size: 14.0, color: SECTION_COLOR },
                        ));
                        spawn_separator(body);
                    }

                    spawn_section_title(body, &font, "Attributes");
                    spawn_stat_header(body, &font);
                    spawn_stat_row(body, &font, "Strength", stats.base_attributes.strength as f32, stats.equipment_attributes.strength as f32, stats.attributes.strength as f32, 0);
                    spawn_stat_row(body, &font, "Dexterity", stats.base_attributes.dexterity as f32, stats.equipment_attributes.dexterity as f32, stats.attributes.dexterity as f32, 0);
                    spawn_stat_row(body, &font, "Agility", stats.base_attributes.agility as f32, stats.equipment_attributes.agility as f32, stats.attributes.agility as f32, 0);
                    spawn_stat_row(body, &font, "Intelligence", stats.base_attributes.intelligence as f32, stats.equipment_attributes.intelligence as f32, stats.attributes.intelligence as f32, 0);
                    spawn_stat_row(body, &font, "Wisdom", stats.base_attributes.wisdom as f32, stats.equipment_attributes.wisdom as f32, stats.attributes.wisdom as f32, 0);
                    spawn_stat_row(body, &font, "Vitality", stats.base_attributes.vitality as f32, stats.equipment_attributes.vitality as f32, stats.attributes.vitality as f32, 0);

                    spawn_separator(body);
                    spawn_section_title(body, &font, "Stats");
                    spawn_stat_header(body, &font);
                    spawn_stat_row(body, &font, "Attack (ATT)", stats.natural.att, stats.equipment.att, stats.total.att, 1);
                    spawn_stat_row(body, &font, "Magic Attack (MATT)", stats.natural.matt, stats.equipment.matt, stats.total.matt, 1);
                    spawn_stat_row(body, &font, "Defense (DEF)", stats.natural.def, stats.equipment.def, stats.total.def, 1);
                    spawn_stat_row(body, &font, "Magic Defense (MDEF)", stats.natural.mdef, stats.equipment.mdef, stats.total.mdef, 1);
                    spawn_stat_row(body, &font, "Crit Chance %", stats.natural.crit_chance, stats.equipment.crit_chance, stats.total.crit_chance, 2);
                    spawn_stat_row(body, &font, "Crit Damage %", stats.natural.crit_damage, stats.equipment.crit_damage, stats.total.crit_damage, 2);
                    spawn_stat_row(body, &font, "Attack Speed %", stats.natural.attack_speed, stats.equipment.attack_speed, stats.total.attack_speed, 2);
                    spawn_stat_row(body, &font, "Cast Speed %", stats.natural.cast_speed, stats.equipment.cast_speed, stats.total.cast_speed, 2);
                    spawn_stat_row(body, &font, "Move Speed %", stats.natural.move_speed_bonus, stats.equipment.move_speed_bonus, stats.total.move_speed_bonus, 2);
                    spawn_stat_row(body, &font, "Cooldown Reduction %", stats.natural.cooldown_reduction, stats.equipment.cooldown_reduction, stats.total.cooldown_reduction, 2);
                    spawn_stat_row(body, &font, "Max HP Bonus", stats.natural.max_health_bonus as f32, stats.equipment.max_health_bonus as f32, stats.total.max_health_bonus as f32, 0);
                    spawn_stat_row(body, &font, "Max MP Bonus", stats.natural.max_mana_bonus as f32, stats.equipment.max_mana_bonus as f32, stats.total.max_mana_bonus as f32, 0);
                    spawn_stat_row(body, &font, "HP Regen /s", stats.natural.hp_regen, stats.equipment.hp_regen, stats.total.hp_regen, 2);
                    spawn_stat_row(body, &font, "MP Regen /s", stats.natural.mp_regen, stats.equipment.mp_regen, stats.total.mp_regen, 2);
                    spawn_stat_row(body, &font, "Weight Capacity", stats.natural.weight_capacity, stats.equipment.weight_capacity, stats.total.weight_capacity, 1);
                });
        });
}

fn spawn_section_title(parent: &mut ChildBuilder, font: &Handle<Font>, title: &str) {
    parent.spawn(TextBundle::from_section(
        title,
        TextStyle { font: font.clone(), font_size: 14.0, color: SECTION_COLOR },
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

fn spawn_stat_header(parent: &mut ChildBuilder, font: &Handle<Font>) {
    parent
        .spawn(NodeBundle {
            style: Style { flex_direction: FlexDirection::Row, column_gap: Val::Px(8.0), ..default() },
            ..default()
        })
        .with_children(|row| {
            spawn_cell(row, font, "", 180.0, DIM_COLOR);
            spawn_cell(row, font, "Base", 60.0, DIM_COLOR);
            spawn_cell(row, font, "Extra", 60.0, DIM_COLOR);
            spawn_cell(row, font, "Total", 60.0, DIM_COLOR);
        });
}

fn spawn_stat_row(parent: &mut ChildBuilder, font: &Handle<Font>, label: &str, base: f32, extra: f32, total: f32, decimals: usize) {
    parent
        .spawn(NodeBundle {
            style: Style { flex_direction: FlexDirection::Row, column_gap: Val::Px(8.0), ..default() },
            ..default()
        })
        .with_children(|row| {
            spawn_cell(row, font, label, 180.0, LABEL_COLOR);
            spawn_cell(row, font, &format!("{base:.decimals$}"), 60.0, VALUE_COLOR);
            spawn_cell(row, font, &format!("{extra:.decimals$}"), 60.0, VALUE_COLOR);
            spawn_cell(row, font, &format!("{total:.decimals$}"), 60.0, VALUE_COLOR);
        });
}

fn spawn_cell(parent: &mut ChildBuilder, font: &Handle<Font>, text: &str, width: f32, color: Color) {
    parent.spawn(NodeBundle { style: Style { width: Val::Px(width), ..default() }, ..default() }).with_children(|cell| {
        cell.spawn(TextBundle::from_section(text, TextStyle { font: font.clone(), font_size: 11.0, color }));
    });
}

fn handle_close_button(
    mut window: ResMut<CharacterStatsWindow>,
    mut buttons: Query<(&Interaction, &mut BackgroundColor), (With<CloseButton>, Changed<Interaction>)>,
) {
    for (interaction, mut background) in &mut buttons {
        *background = match interaction {
            Interaction::Pressed => BUTTON_BG_PRESSED,
            Interaction::Hovered => BUTTON_BG_HOVERED,
            Interaction::None => BUTTON_BG,
        }
        .into();
        if *interaction == Interaction::Pressed {
            window.open = false;
        }
    }
}

/// Same "Escape backs out of the current UI" role `client::interact::
/// close_container_on_cancel` already gives the loot window -- see that
/// system's own doc.
/// `pub(crate)` so `client::chat_ui::handle_escape_key` can order itself
/// after this -- see that system's own doc for why that ordering (not
/// just a `ChatWindow` guard here) is what keeps chat's own Escape-close
/// from also closing this window on the same press.
pub(crate) fn close_on_cancel(
    keyboard: Res<ButtonInput<KeyCode>>,
    input_config: Res<InputConfig>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    mut window: ResMut<CharacterStatsWindow>,
) {
    if !chat_window.open && window.open && input_config.action_just_pressed(&keyboard, PlayerAction::Cancel) {
        window.open = false;
    }
}
