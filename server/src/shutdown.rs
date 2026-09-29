//! Stopping the server cleanly. On Ctrl+C, or the stop signal Docker and
//! service managers send (SIGTERM), every character in the world is saved
//! -- players online and characters left behind by an unsafe disconnect
//! (`Abandoned`) alike -- the save queue is written out, and connected
//! clients are told the server closed, instead of the process dying with
//! up to a minute (`persistence::AUTOSAVE_INTERVAL_SECS`) of their
//! progress unsaved. A second Ctrl+C quits at once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy_renet::renet::transport::NetcodeServerTransport;
use bevy_renet::renet::RenetServer;

use crate::persistence::{SaveQueue, SavedCharacter};

/// How long the last saves may take -- inside the 10 s Docker waits
/// before it kills a container that hasn't stopped.
const SAVE_TIMEOUT: Duration = Duration::from_secs(8);

pub struct ShutdownPlugin;

impl Plugin for ShutdownPlugin {
    fn build(&self, app: &mut App) {
        let requested = Arc::new(AtomicBool::new(false));
        let flag = requested.clone();
        let installed = ctrlc::set_handler(move || {
            if flag.swap(true, Ordering::SeqCst) {
                eprintln!("[server] stopped again -- quitting without saving");
                std::process::exit(1);
            }
        });
        if let Err(e) = installed {
            eprintln!("[server] can't catch stop signals ({e}) -- stopping the server won't save first");
        }
        app.insert_resource(StopRequested(requested));
        // At the end of a frame, so the saves include everything it did.
        app.add_systems(Last, stop_when_requested);
    }
}

/// Set from the signal handler's own thread.
#[derive(Resource)]
struct StopRequested(Arc<AtomicBool>);

fn stop_when_requested(
    requested: Res<StopRequested>,
    characters: Query<SavedCharacter>,
    saves: Res<SaveQueue>,
    mut server: ResMut<RenetServer>,
    mut transport: ResMut<NetcodeServerTransport>,
    mut exit: EventWriter<AppExit>,
) {
    if !requested.0.load(Ordering::SeqCst) {
        return;
    }
    let mut saved = 0;
    for character in &characters {
        saves.save(&character.name.0, character.to_save());
        saved += 1;
    }
    let written = saves.flush_within(SAVE_TIMEOUT);
    // Sent right away, so players see the server close instead of timing
    // out a few seconds later.
    transport.disconnect_all(&mut server);
    // Not println!: when stdout is a pipe whose reader already quit (the
    // `cargo dev` runner, on Ctrl+C), println! panics -- and this is the
    // one moment a panic would cost something.
    let report = if written {
        format!("[server] stopped -- saved {saved} character(s)")
    } else {
        format!("[server] stopped -- {saved} character(s) queued, but the saves didn't finish writing in time")
    };
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{report}");
    exit.send(AppExit);
}
