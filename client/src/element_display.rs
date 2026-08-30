//! A small icon at the top-center of the screen showing which
//! `game_core::ability::ElementAttribute` the local player currently has
//! primed (see `game_core::components::PendingElement`) -- hidden
//! whenever nothing's primed. `PendingElement` is set/cleared by
//! `game_core::systems::combat::trigger_abilities`, which runs in the
//! shared `FixedUpdate` chain on the client the same way it does on the
//! server, so this reads the local player's own predicted copy directly
//! (same "predict locally, no round trip" story `charge_display` already
//! tells) -- no networking of this at all, since it's a purely personal
//! HUD element, not something other players need to see.

use bevy::prelude::*;
use game_core::ability::ElementAttribute;
use game_core::components::PendingElement;

use crate::net::LocalPlayerMarker;

const ICON_SIZE_PX: f32 = 40.0;
/// See `hud.rs`'s own doc for the confirmed `left`/`right`-anchoring
/// dead zone this sidesteps: the icon is centered via this full-width
/// parent's own `justify_content` (parent stays at `left: 0`, always
/// safe) rather than by giving the icon itself a computed `left`/`right`.
const TOP_PX: f32 = 8.0;

fn icon_path(element: ElementAttribute) -> &'static str {
    match element {
        ElementAttribute::Fire => "magic/icons/fire_icon.png",
        ElementAttribute::Water => "magic/icons/water_icon.png",
        ElementAttribute::Earth => "magic/icons/earth_icon.png",
        ElementAttribute::Wind => "magic/icons/wind_icon.png",
    }
}

#[derive(Component)]
struct PendingElementIcon;

pub struct ElementDisplayPlugin;

impl Plugin for ElementDisplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_element_display);
        app.add_systems(Update, update_element_display);
    }
}

fn spawn_element_display(mut commands: Commands) {
    commands
        .spawn(NodeBundle {
            style: Style {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(TOP_PX),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
            ..default()
        })
        .with_children(|parent| {
            parent.spawn((
                PendingElementIcon,
                ImageBundle {
                    style: Style {
                        width: Val::Px(ICON_SIZE_PX),
                        height: Val::Px(ICON_SIZE_PX),
                        ..default()
                    },
                    visibility: Visibility::Hidden,
                    ..default()
                },
            ));
        });
}

/// Swaps the icon's own texture (only when the primed element actually
/// changes, via `Changed<PendingElement>`/removal -- comparing against a
/// full `Option<&PendingElement>` query every frame regardless) and shows/
/// hides it. Both the local player having no `PendingElement` at all and
/// it having just been removed this frame read the same way: hidden.
fn update_element_display(
    asset_server: Res<AssetServer>,
    local_player: Query<Option<&PendingElement>, With<LocalPlayerMarker>>,
    mut icon: Query<(&mut UiImage, &mut Visibility), With<PendingElementIcon>>,
) {
    let Ok(pending) = local_player.get_single() else { return };
    let Ok((mut image, mut visibility)) = icon.get_single_mut() else { return };
    match pending {
        Some(PendingElement(element)) => {
            image.texture = asset_server.load(icon_path(*element));
            *visibility = Visibility::Visible;
        }
        None => {
            *visibility = Visibility::Hidden;
        }
    }
}
