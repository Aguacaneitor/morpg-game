//! What a player sees when the connection to the game server ends while
//! they're in the world: the server shut down (it says so as it goes --
//! see `server::shutdown`), or it stopped answering. Before this the world
//! just froze with no explanation, since the login screen never comes back
//! once a character is in play. Offers "Close Game", like `death_screen`.

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy_renet::renet::transport::{NetcodeDisconnectReason, NetcodeError, NetcodeTransportError};
use bevy_renet::renet::RenetClient;

use crate::net::LocalPlayer;

const OVERLAY_BG: Color = Color::rgba(0.02, 0.02, 0.05, 0.88);
const TITLE_COLOR: Color = Color::rgb(0.85, 0.78, 0.60);
const MESSAGE_COLOR: Color = Color::rgb(0.82, 0.82, 0.82);
const BUTTON_BG: Color = Color::rgb(0.20, 0.16, 0.10);
const BUTTON_BG_HOVERED: Color = Color::rgb(0.30, 0.24, 0.15);
const BUTTON_BG_PRESSED: Color = Color::rgb(0.42, 0.34, 0.20);
const BUTTON_TEXT_COLOR: Color = Color::rgb(0.85, 0.78, 0.60);
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";
const LOST: &str = "The connection to the server was lost.";

#[derive(Component)]
struct DisconnectScreenRoot;

#[derive(Component)]
struct DisconnectMessage;

#[derive(Component)]
struct CloseGameButton;

/// Why the connection ended, as the transport reported it -- that arrives
/// as an event, while the screen stays up.
#[derive(Resource, Default)]
struct WhyDisconnected(Option<&'static str>);

pub struct DisconnectScreenPlugin;

impl Plugin for DisconnectScreenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WhyDisconnected>();
        app.add_systems(Startup, spawn_disconnect_screen);
        app.add_systems(Update, (note_why, show_when_disconnected, close_game_button).chain());
    }
}

fn spawn_disconnect_screen(mut commands: Commands, asset_server: Res<AssetServer>) {
    let font = asset_server.load(UI_FONT);
    commands
        .spawn((
            DisconnectScreenRoot,
            NodeBundle {
                style: Style {
                    display: Display::None,
                    position_type: PositionType::Absolute,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(18.0),
                    ..default()
                },
                background_color: OVERLAY_BG.into(),
                // Above the death screen (1000): being disconnected trumps it.
                z_index: ZIndex::Global(1100),
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn(TextBundle::from_section(
                "Disconnected",
                TextStyle { font: font.clone(), font_size: 36.0, color: TITLE_COLOR },
            ));
            root.spawn((
                DisconnectMessage,
                TextBundle::from_section(LOST, TextStyle { font: font.clone(), font_size: 18.0, color: MESSAGE_COLOR }),
            ));
            root.spawn((
                CloseGameButton,
                NodeBundle {
                    style: Style { padding: UiRect::axes(Val::Px(28.0), Val::Px(12.0)), ..default() },
                    background_color: BUTTON_BG.into(),
                    ..default()
                },
                Interaction::default(),
            ))
            .with_children(|button| {
                button.spawn(TextBundle::from_section(
                    "Close Game",
                    TextStyle { font: font.clone(), font_size: 20.0, color: BUTTON_TEXT_COLOR },
                ));
            });
        });
}

fn note_why(mut errors: EventReader<NetcodeTransportError>, mut why: ResMut<WhyDisconnected>) {
    for error in errors.read() {
        if let NetcodeTransportError::Netcode(NetcodeError::Disconnected(reason)) = error {
            why.0 = Some(match reason {
                NetcodeDisconnectReason::DisconnectedByServer => "The server closed the connection.",
                NetcodeDisconnectReason::ConnectionTimedOut => "The server stopped responding.",
                _ => LOST,
            });
        }
    }
}

/// Only once a character is in play -- before that, login and character
/// select show their own errors.
fn show_when_disconnected(
    client: Res<RenetClient>,
    local_player: Option<Res<LocalPlayer>>,
    why: Res<WhyDisconnected>,
    mut root: Query<&mut Style, With<DisconnectScreenRoot>>,
    mut message: Query<&mut Text, With<DisconnectMessage>>,
) {
    let Ok(mut style) = root.get_single_mut() else { return };
    let shown = local_player.is_some() && client.is_disconnected();
    let display = if shown { Display::Flex } else { Display::None };
    if style.display != display {
        style.display = display;
    }
    if !shown {
        return;
    }
    let Ok(mut text) = message.get_single_mut() else { return };
    let wanted = why.0.unwrap_or(LOST);
    if text.sections[0].value != wanted {
        text.sections[0].value = wanted.to_string();
    }
}

fn close_game_button(
    mut buttons: Query<(&Interaction, &mut BackgroundColor), (Changed<Interaction>, With<CloseGameButton>)>,
    mut app_exit: EventWriter<AppExit>,
) {
    for (interaction, mut background) in &mut buttons {
        *background = match interaction {
            Interaction::Pressed => BUTTON_BG_PRESSED,
            Interaction::Hovered => BUTTON_BG_HOVERED,
            Interaction::None => BUTTON_BG,
        }
        .into();
        if *interaction == Interaction::Pressed {
            app_exit.send(AppExit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_renet::renet::ConnectionConfig;
    use game_core::components::NetworkId;

    fn shown(app: &App) -> (Display, String) {
        let display = app.world.iter_entities().find_map(|e| e.get::<DisconnectScreenRoot>().and(e.get::<Style>())).unwrap().display;
        let text = app.world.iter_entities().find_map(|e| e.get::<DisconnectMessage>().and(e.get::<Text>())).unwrap();
        (display, text.sections[0].value.clone())
    }

    #[test]
    fn shows_why_once_a_character_is_in_play() {
        let mut app = App::new();
        app.add_event::<NetcodeTransportError>();
        app.init_resource::<WhyDisconnected>();
        let mut client = RenetClient::new(ConnectionConfig::default());
        client.disconnect();
        app.insert_resource(client);
        app.world.spawn((DisconnectScreenRoot, Style { display: Display::None, ..default() }));
        app.world.spawn((DisconnectMessage, Text::from_section(LOST, TextStyle::default())));
        app.add_systems(Update, (note_why, show_when_disconnected).chain());

        app.update();
        assert_eq!(shown(&app).0, Display::None, "not in the world yet: login shows its own errors");

        app.insert_resource(LocalPlayer { network_id: NetworkId(1), entity: Entity::PLACEHOLDER });
        app.world.send_event(NetcodeTransportError::Netcode(NetcodeError::Disconnected(
            NetcodeDisconnectReason::DisconnectedByServer,
        )));
        app.update();
        assert_eq!(shown(&app), (Display::Flex, "The server closed the connection.".to_string()));
    }
}
