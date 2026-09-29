//! The resource bars along the bottom of the game view: Health and Stamina
//! bottom-left, Mana and Faith bottom-right, and the Experience bar
//! stretched between them. Art lives in `gallery/UI/bars/`.
//!
//! A vertical bar is its art 9-sliced (borders L7 R6 T32 B32, `POOL_ART`)
//! to `POOL_BAR_HEIGHT` tall, so the label and the icon keep their pixels
//! and only the middle of the tube stretches. The live fill is laid over
//! the tube (the column between the left and right borders): the tube
//! drawn empty, then filled from the bottom to the current fraction with
//! the art's own fill row, topped by the art's own surface highlight.
//! Both are read from each image once it loads (`check_loaded_art`), so
//! redrawn art keeps working as long as its borders stay the same.
//!
//! The Experience bar has words at three spots (EXP, EXPERIENCE, LVL), so
//! instead of 9-slicing it stretches two plain one-pixel columns between
//! them (`EXP_ART`): every word stays unstretched and the title centered.
//!
//! Sizes are art pixels times `HUD_SCALE`, a whole number: Bevy 0.13 rounds
//! UI layout to whole logical pixels, so a fractional scale opens gaps
//! between the slices. On a display scaled 125% that makes an art pixel 2.5
//! physical pixels, so some come out a pixel wider than others -- and a
//! sampled edge can land half a pixel off the fill laid over it, which is
//! why the frame is drawn from a copy of the art with its track painted
//! empty (`check_loaded_art`): whatever peeks out around the live fill is
//! empty tube, never the art's own drawn fill. The art is sampled nearest
//! so it stays crisp. Laid out with flex rather than `right:`/`bottom:`
//! offsets -- see `hud.rs`'s own doc for why.

use bevy::prelude::*;
use bevy::render::texture::{ImageLoaderSettings, ImageSampler};
use game_core::components::{CharacterLevel, Faith, Health, Mana, Stamina};
use game_core::profession::xp_required_for_level;

use crate::net::LocalPlayerMarker;
use crate::ui::SIDEBAR_WIDTH;

/// Logical pixels per art pixel -- keep it a whole number (see this
/// module's own doc).
const HUD_SCALE: f32 = 2.0;
/// A vertical bar's full height, in art pixels -- at least its top and
/// bottom borders (64).
const POOL_BAR_HEIGHT: u32 = 88;
/// Space between the bars and the edges of the game view, in art pixels.
const EDGE_MARGIN: u32 = 4;
/// Space between two bars side by side, and around the Experience bar, in
/// art pixels.
const BAR_GAP: u32 = 3;
const HUD_FONT: &str = "fonts/FiraMono-subset.ttf";
const VALUE_TEXT_COLOR: Color = Color::rgb(0.92, 0.9, 0.86);
const VALUE_TEXT_BG: Color = Color::rgba(0.0, 0.0, 0.0, 0.6);
/// The tallest surface highlight looked for above an art's fill.
const MAX_SURFACE_ROWS: u32 = 8;

/// One axis of a slicing: `(start, length, stretches)` in art pixels,
/// together covering the image edge to edge.
type Slices = &'static [(u32, u32, bool)];

/// How one piece of bar art is cut up and where its fill goes, in art
/// pixels.
struct BarArt {
    size: UVec2,
    columns: Slices,
    rows: Slices,
    /// The area the live fill covers, `min` inclusive, `max` exclusive.
    track: URect,
    /// A one-pixel line across the track as drawn empty, and as drawn full.
    empty_sample: URect,
    fill_sample: URect,
    fills_up: bool,
}

/// Health, Stamina, Mana and Faith: 22x126, borders L7 R6 T32 B32; the
/// tube runs from row 17 down to the bottom border, row 20 is always
/// empty and the middle slice always full.
const POOL_ART: BarArt = BarArt {
    size: UVec2::new(22, 126),
    columns: &[(0, 7, false), (7, 9, true), (16, 6, false)],
    rows: &[(0, 32, false), (32, 62, true), (94, 32, false)],
    track: URect { min: UVec2::new(7, 17), max: UVec2::new(16, 94) },
    empty_sample: URect { min: UVec2::new(7, 20), max: UVec2::new(16, 21) },
    fill_sample: URect { min: UVec2::new(7, 63), max: UVec2::new(16, 64) },
    fills_up: true,
};

/// Experience: 134x22. Columns 43 and 106 are plain (between EXP and
/// EXPERIENCE, and between EXPERIENCE and LVL); the track is rows 11-16.
const EXP_ART: BarArt = BarArt {
    size: UVec2::new(134, 22),
    columns: &[(0, 43, false), (43, 1, true), (44, 62, false), (106, 1, true), (107, 27, false)],
    rows: &[(0, 22, false)],
    track: URect { min: UVec2::new(7, 11), max: UVec2::new(128, 17) },
    empty_sample: URect { min: UVec2::new(110, 11), max: UVec2::new(111, 17) },
    fill_sample: URect { min: UVec2::new(60, 11), max: UVec2::new(61, 17) },
    fills_up: false,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Meter {
    Health,
    Stamina,
    Mana,
    Faith,
    Experience,
}

impl Meter {
    fn art_path(self) -> &'static str {
        match self {
            Meter::Health => "UI/bars/Health_bar.png",
            Meter::Stamina => "UI/bars/Stamina_bar.png",
            Meter::Mana => "UI/bars/Mana_bar.png",
            Meter::Faith => "UI/bars/Faith_bar.png",
            Meter::Experience => "UI/bars/Experience_bar.png",
        }
    }

    fn art(self) -> &'static BarArt {
        match self {
            Meter::Experience => &EXP_ART,
            _ => &POOL_ART,
        }
    }
}

/// Everything the bars are drawn in; hidden until there's a local player.
#[derive(Component)]
struct HudBarsRoot;

/// The live fill of a meter -- sized to the current fraction.
#[derive(Component)]
struct MeterFill(Meter);

/// A meter's numbers.
#[derive(Component)]
struct MeterText(Meter);

/// On a bar until its art has loaded -- see `check_loaded_art`.
#[derive(Component)]
struct ArtCheck {
    meter: Meter,
    image: Handle<Image>,
    layout: Handle<TextureAtlasLayout>,
    /// The slice cells drawing the frame.
    cells: Vec<Entity>,
    /// A pool bar's surface highlight node.
    surface: Option<Entity>,
}

pub struct HudBarsPlugin;

impl Plugin for HudBarsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_bars);
        app.add_systems(Update, (check_loaded_art, update_meters));
    }
}

fn spawn_bars(mut commands: Commands, asset_server: Res<AssetServer>, mut layouts: ResMut<Assets<TextureAtlasLayout>>) {
    let font: Handle<Font> = asset_server.load(HUD_FONT);
    let px = |art_px: u32| Val::Px(art_px as f32 * HUD_SCALE);
    // Full screen, laid out like `ui.rs`'s root: the game view, then room
    // for the sidebar. Absolute, so it doesn't share the screen with that
    // root in flex.
    let root = commands
        .spawn((
            HudBarsRoot,
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    ..default()
                },
                visibility: Visibility::Hidden,
                // Under every window (they use 50 and up).
                z_index: ZIndex::Global(10),
                ..default()
            },
        ))
        .id();
    let view = commands
        .spawn(NodeBundle {
            style: Style {
                flex_grow: 1.0,
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::FlexEnd,
                padding: UiRect::all(px(EDGE_MARGIN)),
                column_gap: px(BAR_GAP),
                ..default()
            },
            ..default()
        })
        .id();
    let sidebar_room = commands
        .spawn(NodeBundle { style: Style { width: Val::Px(SIDEBAR_WIDTH), flex_shrink: 0.0, ..default() }, ..default() })
        .id();
    commands.entity(root).push_children(&[view, sidebar_room]);

    let mut spawn_meter = |commands: &mut Commands, parent: Entity, meter: Meter| {
        let art = meter.art();
        let image = asset_server
            .load_with_settings(meter.art_path(), |settings: &mut ImageLoaderSettings| settings.sampler = ImageSampler::nearest());
        let (layout, empty_index, fill_index) = atlas_for(art, &mut layouts);
        // The numbers over the bar; the Experience bar takes all the room
        // between the two pairs.
        let column = commands
            .spawn(NodeBundle {
                style: Style {
                    flex_grow: if art.fills_up { 0.0 } else { 1.0 },
                    margin: if art.fills_up { UiRect::default() } else { UiRect::horizontal(px(BAR_GAP)) },
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(2.0),
                    ..default()
                },
                ..default()
            })
            .id();
        commands.entity(parent).add_child(column);
        let text = commands
            .spawn((
                MeterText(meter),
                TextBundle::from_section("", TextStyle { font: font.clone(), font_size: 10.0, color: VALUE_TEXT_COLOR })
                    .with_style(Style { padding: UiRect::horizontal(Val::Px(2.0)), ..default() })
                    .with_background_color(VALUE_TEXT_BG),
            ))
            .id();
        let bar = commands
            .spawn(NodeBundle {
                style: Style {
                    width: if art.fills_up { px(art.size.x) } else { Val::Percent(100.0) },
                    height: if art.fills_up { px(POOL_BAR_HEIGHT) } else { px(art.size.y) },
                    flex_direction: FlexDirection::Column,
                    ..default()
                },
                ..default()
            })
            .id();
        commands.entity(column).push_children(&[text, bar]);
        let cells = spawn_slices(commands, bar, art, &image, &layout);
        let surface = spawn_track(commands, bar, meter, &image, &layout, [empty_index, fill_index]);
        commands.entity(bar).insert(ArtCheck { meter, image, layout, cells, surface });
    };

    let left = commands.spawn(pair_node()).id();
    commands.entity(view).add_child(left);
    spawn_meter(&mut commands, left, Meter::Health);
    spawn_meter(&mut commands, left, Meter::Stamina);
    spawn_meter(&mut commands, view, Meter::Experience);
    let right = commands.spawn(pair_node()).id();
    commands.entity(view).add_child(right);
    spawn_meter(&mut commands, right, Meter::Mana);
    spawn_meter(&mut commands, right, Meter::Faith);
}

/// Two vertical bars side by side, bottoms lined up.
fn pair_node() -> NodeBundle {
    NodeBundle {
        style: Style {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::FlexEnd,
            column_gap: Val::Px(BAR_GAP as f32 * HUD_SCALE),
            ..default()
        },
        ..default()
    }
}

/// One rect per slice cell (row by row, from index 0), then the empty and
/// fill samples -- whose indices are returned with the layout.
fn atlas_for(art: &BarArt, layouts: &mut Assets<TextureAtlasLayout>) -> (Handle<TextureAtlasLayout>, usize, usize) {
    let mut layout = TextureAtlasLayout::new_empty(art.size.as_vec2());
    for &(y, height, _) in art.rows {
        for &(x, width, _) in art.columns {
            layout.add_texture(Rect::new(x as f32, y as f32, (x + width) as f32, (y + height) as f32));
        }
    }
    let empty = layout.add_texture(urect_to_rect(art.empty_sample));
    let fill = layout.add_texture(urect_to_rect(art.fill_sample));
    (layouts.add(layout), empty, fill)
}

fn urect_to_rect(rect: URect) -> Rect {
    Rect::new(rect.min.x as f32, rect.min.y as f32, rect.max.x as f32, rect.max.y as f32)
}

/// A node drawing `index` of `layout` -- stretched to whatever size
/// `style` gives it.
fn atlas_node(style: Style, image: &Handle<Image>, layout: &Handle<TextureAtlasLayout>, index: usize) -> impl Bundle {
    (
        // White: a node's background color tints its image.
        NodeBundle { style, background_color: Color::WHITE.into(), ..default() },
        UiImage::new(image.clone()),
        TextureAtlas { layout: layout.clone(), index },
    )
}

/// Fills `bar` with the art's slice cells -- fixed ones at their size
/// times `HUD_SCALE`, stretching ones sharing whatever room is left -- and
/// returns them.
fn spawn_slices(
    commands: &mut Commands,
    bar: Entity,
    art: &BarArt,
    image: &Handle<Image>,
    layout: &Handle<TextureAtlasLayout>,
) -> Vec<Entity> {
    let mut cells = Vec::new();
    for &(_, height, row_stretches) in art.rows {
        let row = commands
            .spawn(NodeBundle {
                style: Style {
                    flex_direction: FlexDirection::Row,
                    width: Val::Percent(100.0),
                    height: if row_stretches { Val::Auto } else { Val::Px(height as f32 * HUD_SCALE) },
                    flex_grow: if row_stretches { 1.0 } else { 0.0 },
                    flex_shrink: 0.0,
                    flex_basis: if row_stretches { Val::Px(0.0) } else { Val::Auto },
                    min_height: Val::Px(0.0),
                    ..default()
                },
                ..default()
            })
            .id();
        commands.entity(bar).add_child(row);
        for &(_, width, column_stretches) in art.columns {
            let style = Style {
                width: if column_stretches { Val::Auto } else { Val::Px(width as f32 * HUD_SCALE) },
                height: Val::Percent(100.0),
                flex_grow: if column_stretches { 1.0 } else { 0.0 },
                flex_shrink: 0.0,
                flex_basis: if column_stretches { Val::Px(0.0) } else { Val::Auto },
                min_width: Val::Px(0.0),
                ..default()
            };
            let cell = commands.spawn(atlas_node(style, image, layout, cells.len())).id();
            commands.entity(row).add_child(cell);
            cells.push(cell);
        }
    }
    cells
}

/// Lays the live fill over the art's track: the track drawn empty, and in
/// it the fill (`MeterFill`) growing from the bottom (or the left). A pool
/// bar's fill is topped by a surface highlight node, returned -- sized
/// once the art has loaded (`check_loaded_art`).
fn spawn_track(
    commands: &mut Commands,
    bar: Entity,
    meter: Meter,
    image: &Handle<Image>,
    layout: &Handle<TextureAtlasLayout>,
    [empty_index, fill_index]: [usize; 2],
) -> Option<Entity> {
    let art = meter.art();
    let px = |art_px: u32| Val::Px(art_px as f32 * HUD_SCALE);
    // Padding rather than right/bottom offsets puts the track in place.
    let overlay = commands
        .spawn(NodeBundle {
            style: Style {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                padding: UiRect {
                    left: px(art.track.min.x),
                    right: px(art.size.x - art.track.max.x),
                    top: px(art.track.min.y),
                    bottom: px(art.size.y - art.track.max.y),
                },
                ..default()
            },
            ..default()
        })
        .id();
    let track = commands
        .spawn(atlas_node(
            Style {
                flex_grow: 1.0,
                flex_direction: if art.fills_up { FlexDirection::Column } else { FlexDirection::Row },
                justify_content: if art.fills_up { JustifyContent::FlexEnd } else { JustifyContent::FlexStart },
                ..default()
            },
            image,
            layout,
            empty_index,
        ))
        .id();
    commands.entity(bar).add_child(overlay);
    commands.entity(overlay).add_child(track);
    if !art.fills_up {
        let fill = commands
            .spawn((
                MeterFill(meter),
                atlas_node(Style { width: Val::Percent(0.0), height: Val::Percent(100.0), ..default() }, image, layout, fill_index),
            ))
            .id();
        commands.entity(track).add_child(fill);
        return None;
    }
    let fill = commands
        .spawn((
            MeterFill(meter),
            NodeBundle {
                style: Style {
                    width: Val::Percent(100.0),
                    height: Val::Percent(0.0),
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::clip(),
                    ..default()
                },
                ..default()
            },
        ))
        .id();
    // Zero tall, showing the fill row, until the art has loaded.
    let surface = commands
        .spawn(atlas_node(
            Style { width: Val::Percent(100.0), height: Val::Px(0.0), flex_shrink: 0.0, ..default() },
            image,
            layout,
            fill_index,
        ))
        .id();
    let body = commands
        .spawn(atlas_node(
            Style { width: Val::Percent(100.0), flex_grow: 1.0, flex_basis: Val::Px(0.0), ..default() },
            image,
            layout,
            fill_index,
        ))
        .id();
    commands.entity(track).add_child(fill);
    commands.entity(fill).push_children(&[surface, body]);
    Some(surface)
}

/// Once a bar's art has loaded: warns if it isn't the size the slicing was
/// written for; draws the frame from a copy of it with the track painted
/// empty (see this module's own doc); and fits a pool bar's surface
/// highlight to the art's own -- the rows between the empty part of the
/// drawn tube and its full part.
fn check_loaded_art(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut layouts: ResMut<Assets<TextureAtlasLayout>>,
    pending: Query<(Entity, &ArtCheck)>,
    mut cells: Query<&mut UiImage>,
    mut surfaces: Query<(&mut TextureAtlas, &mut Style)>,
) {
    for (entity, check) in &pending {
        let Some(image) = images.get(&check.image) else { continue };
        commands.entity(entity).remove::<ArtCheck>();
        let art = check.meter.art();
        if image.size() != art.size {
            warn!(
                "[hud] {} is {}x{}, the bars expect {}x{} -- its slices will be off",
                check.meter.art_path(),
                image.size().x,
                image.size().y,
                art.size.x,
                art.size.y
            );
            continue;
        }
        let surface_rows = if check.surface.is_some() { surface_rows(image, art) } else { None };
        let frame = with_empty_track(image, art);
        let frame = images.add(frame);
        for &cell in &check.cells {
            if let Ok(mut cell_image) = cells.get_mut(cell) {
                cell_image.texture = frame.clone();
            }
        }
        let (Some(surface), Some(rows)) = (check.surface, surface_rows) else { continue };
        let (Some(layout), Ok((mut atlas, mut style))) = (layouts.get_mut(&check.layout), surfaces.get_mut(surface)) else {
            continue;
        };
        atlas.index = layout.add_texture(Rect::new(
            art.track.min.x as f32,
            rows.start as f32,
            art.track.max.x as f32,
            rows.end as f32,
        ));
        style.height = Val::Px(rows.len() as f32 * HUD_SCALE);
    }
}

/// The RGBA bytes of art pixel `(x, y)`.
fn pixel(image: &Image, art: &BarArt, x: u32, y: u32) -> Option<[u8; 4]> {
    let at = ((y * art.size.x + x) * 4) as usize;
    image.data.get(at..at + 4)?.try_into().ok()
}

/// A copy of `image` with its track painted the way it looks empty.
fn with_empty_track(image: &Image, art: &BarArt) -> Image {
    let mut frame = image.clone();
    for y in art.track.min.y..art.track.max.y {
        for x in art.track.min.x..art.track.max.x {
            let empty = if art.fills_up { pixel(image, art, x, art.empty_sample.min.y) } else { pixel(image, art, art.empty_sample.min.x, y) };
            let at = ((y * art.size.x + x) * 4) as usize;
            if let (Some(empty), Some(target)) = (empty, frame.data.get_mut(at..at + 4)) {
                target.copy_from_slice(&empty);
            }
        }
    }
    frame
}

/// The art rows between the empty and the full part of a pool bar's
/// drawn tube, read down its middle column; `None` if there are none (or
/// suspiciously many).
fn surface_rows(image: &Image, art: &BarArt) -> Option<std::ops::Range<u32>> {
    let x = (art.track.min.x + art.track.max.x) / 2;
    let empty = pixel(image, art, x, art.empty_sample.min.y)?;
    let full = pixel(image, art, x, art.fill_sample.min.y)?;
    let start = (art.track.min.y..art.track.max.y).find(|&y| pixel(image, art, x, y) != Some(empty))?;
    let end = (start..art.track.max.y).find(|&y| pixel(image, art, x, y) == Some(full))?;
    (start < end && end - start <= MAX_SURFACE_ROWS).then_some(start..end)
}

/// Sizes every fill and rewrites the numbers, only when a value changed;
/// hides everything while there's no local player (login, character
/// select).
fn update_meters(
    local_player: Query<(&Health, &Stamina, &Mana, &Faith, &CharacterLevel), With<LocalPlayerMarker>>,
    mut root: Query<&mut Visibility, With<HudBarsRoot>>,
    mut fills: Query<(&MeterFill, &mut Style)>,
    mut texts: Query<(&MeterText, &mut Text)>,
    mut shown: Local<Option<([(i32, i32); 5], u32)>>,
) {
    let Ok(mut visibility) = root.get_single_mut() else { return };
    let Ok((health, stamina, mana, faith, level)) = local_player.get_single() else {
        visibility.set_if_neq(Visibility::Hidden);
        *shown = None;
        return;
    };
    visibility.set_if_neq(Visibility::Inherited);
    let values = [
        (health.current, health.max),
        (stamina.current, stamina.max),
        (mana.current, mana.max),
        (faith.current, faith.max),
        (level.xp as i32, xp_required_for_level(level.level) as i32),
    ];
    if *shown == Some((values, level.level)) {
        return;
    }
    *shown = Some((values, level.level));
    let value = |meter: Meter| match meter {
        Meter::Health => values[0],
        Meter::Stamina => values[1],
        Meter::Mana => values[2],
        Meter::Faith => values[3],
        Meter::Experience => values[4],
    };
    for (MeterFill(meter), mut style) in &mut fills {
        let (current, max) = value(*meter);
        let percent = Val::Percent((current.max(0) as f32 / max.max(1) as f32).min(1.0) * 100.0);
        if meter.art().fills_up {
            style.height = percent;
        } else {
            style.width = percent;
        }
    }
    for (MeterText(meter), mut text) in &mut texts {
        let (current, max) = value(*meter);
        text.sections[0].value = match meter {
            Meter::Experience => format!("Level {}  {current} / {max} XP", level.level),
            _ => format!("{}/{max}", current.max(0)),
        };
    }
}
