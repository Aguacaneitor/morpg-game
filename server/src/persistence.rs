//! Character save/load -- SQLite-backed (via `rusqlite`), one row per
//! character, a single RON-serialized blob column rather than a fully normalized
//! schema. The blob approach is deliberate: the exact set of persisted
//! components has already changed several times over this project's own
//! history (this is Phase 1 of a larger persistence/login plan, built
//! before any account system exists), and a serialized blob +
//! `#[serde(default)]` on its own fields -- the same trick this project's
//! own RON data files already lean on everywhere -- absorbs that kind of
//! change for free, where a real column-per-field schema would need a
//! migration every time.
//!
//! Phase 4 wired accounts in: `create_character` stamps `account_id` /
//! `server_id`, and `server::character_select` scopes every list/lookup
//! to them. A row is still addressed by `name` alone in `load_character`
//! / `SaveQueue::save` (autosave and logout only ever know a name), and
//! `name` is still globally `UNIQUE` -- `server_id` is always `1` until
//! real multi-server exists.
//!
//! Saving never runs on the game loop: every save (autosave, logout,
//! disconnect, the abandoned sweep) goes through `SaveQueue` to a
//! background thread with its own connection, which writes whatever is
//! queued in one transaction and keeps retrying failed writes rather than
//! dropping them. Reads stay synchronous on `SaveDb` -- they only happen
//! during character select -- and `SaveQueue::flush` makes sure a
//! character's own just-queued save is on disk before it's loaded back.
//! A database error is logged and turned into a refusal for that one
//! request, never a server panic.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bevy::ecs::query::QueryData;
use bevy::prelude::*;
use rusqlite::{params, Connection, OptionalExtension};

use protocol::CharacterSummary;

use game_core::components::{
    Backpack, CharacterLevel, CharacterRace, Classes, Equipment, KnownAbilities, Level, Position, ProfessionPoints,
    Sex,
};
use game_core::player::PlayerCharacter;
use game_core::states::{CombatState, InstanceId};

/// Where `SaveDb::open` puts the SQLite file by default -- overridable
/// via `ARPG_SAVE_DB_PATH`, same env-var-with-a-default idiom every other
/// data path in this project already uses (`ARPG_GAMEPLAY_CONFIG_PATH`
/// etc.).
pub const DEFAULT_SAVE_DB_PATH: &str = "saves/game.db";

/// Everything a save is built from, as one query: every system that saves
/// a character (autosave, disconnect, logout, the abandoned sweep) queries
/// `SavedCharacter` and calls `to_save`, so the list of saved components
/// exists exactly once.
#[derive(QueryData)]
pub struct SavedCharacter {
    pub name: &'static CharacterName,
    position: &'static Position,
    level: &'static Level,
    instance: &'static InstanceId,
    race: &'static CharacterRace,
    sex: &'static Sex,
    classes: &'static Classes,
    character_level: &'static CharacterLevel,
    profession_points: &'static ProfessionPoints,
    known_abilities: &'static KnownAbilities,
    equipment: &'static Equipment,
    backpack: &'static Backpack,
    combat_state: &'static CombatState,
}

impl SavedCharacterItem<'_> {
    /// This character's state right now, as it gets saved.
    pub fn to_save(&self) -> PlayerCharacter {
        PlayerCharacter {
            position: self.position.clone(),
            level: self.level.clone(),
            instance: self.instance.clone(),
            race: self.race.clone(),
            sex: self.sex.clone(),
            classes: self.classes.clone(),
            character_level: self.character_level.clone(),
            profession_points: self.profession_points.clone(),
            known_abilities: self.known_abilities.clone(),
            equipment: self.equipment.clone(),
            backpack: self.backpack.clone(),
            alive: !matches!(self.combat_state, CombatState::Dead),
        }
    }
}

/// Which save row a connected player entity corresponds to -- server-only
/// bookkeeping, never part of the client's own local-player bundle, same
/// role `components::KillCounts` already plays. Inserted by
/// `server::character_select::handle_character_select` at the moment the
/// player entity is spawned (character picked), so it's always present
/// for the whole life of an in-world entity -- `tick_autosave` and the
/// disconnect-save hook still guard with `if let Ok(..)` regardless.
#[derive(Component, Debug, Clone)]
pub struct CharacterName(pub String);

/// A `Mutex`, not a bare `Connection` -- `rusqlite::Connection` is `Send`
/// but not `Sync`, and Bevy's `Resource` trait requires both
/// unconditionally regardless of whether a resource is ever actually
/// accessed from more than one system at a time. Wrapping it is what
/// makes this compile at all; every system that touches it takes a
/// shared `Res<SaveDb>` and locks briefly inside (no `Arc` needed on top
/// -- Bevy already stores the one `Resource` instance itself, nothing
/// here is ever cloned out to live independently of that).
#[derive(Resource)]
pub struct SaveDb(Mutex<Connection>);

/// Both `SaveDb` and the `SaveQueue` writer open the file this way.
fn open_connection(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    // WAL: reads (character select) never wait on a write in progress, and
    // with NORMAL a commit doesn't wait for a disk flush. Still safe if the
    // server process crashes; only an OS crash or power cut can roll back
    // the newest transactions, never corrupt the file.
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // Two connections write now (this one creates characters, the save
    // writer updates them): wait for the other briefly instead of failing.
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

impl SaveDb {
    /// Startup only -- failing to open or set up the file is fatal here, on
    /// purpose, before anyone has connected.
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create saves directory");
        }
        let conn = open_connection(path).unwrap_or_else(|e| panic!("failed to open save database {path:?}: {e}"));
        conn.execute(
            "CREATE TABLE IF NOT EXISTS characters (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                server_id INTEGER NOT NULL DEFAULT 1,
                account_id INTEGER,
                data TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
            [],
        )
        .expect("failed to create characters table");
        // `name`'s own column-level `UNIQUE` above is case-sensitive
        // (SQLite's default `BINARY` collation) -- it happily lets "Bob"
        // and "bob" coexist. `character_select::handle_character_select`
        // already pre-checks `character_name_taken` (which *is*
        // `COLLATE NOCASE`) before ever inserting, and because that whole
        // create-a-character flow runs synchronously on one thread against
        // this one `Mutex`-guarded connection, two same-tick requests for
        // case-variant names can't actually race each other today. This
        // index is the real, DB-level backstop anyway -- the same
        // "let the database enforce its own invariant" approach
        // `auth_server`'s own `accounts.email UNIQUE COLLATE NOCASE`
        // already uses -- so the guarantee holds even if that processing
        // model ever changes (multiple threads, a second server process
        // pointed at this same file, a future bug in the pre-check).
        // Not `.expect()`ed: unlike the table itself, this could only
        // fail on an *existing* database that already has a case-variant
        // duplicate in it, which is a data problem to report, not a
        // reason to refuse to start.
        if let Err(e) = conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS characters_name_nocase_idx ON characters (name COLLATE NOCASE)",
            [],
        ) {
            eprintln!(
                "[server] warning: could not create the case-insensitive name index ({e}) -- likely an \
                 existing case-variant duplicate (e.g. \"Bob\" and \"bob\") already in {path:?}; new \
                 character names are still checked case-insensitively before creation, just without this \
                 DB-level backstop until the duplicate is resolved"
            );
        }
        Self(Mutex::new(conn))
    }

    /// A panic can't happen while this lock is held any more, so a poisoned
    /// lock carries no broken state -- just keep going.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `Ok(None)` if no character with this name has ever been saved (or its
/// blob won't parse) -- the caller decides what that means. Call
/// `SaveQueue::flush` first if this character may have a save still queued.
pub fn load_character(db: &SaveDb, name: &str) -> rusqlite::Result<Option<PlayerCharacter>> {
    let data: Option<String> = db
        .conn()
        .query_row("SELECT data FROM characters WHERE name = ?1", params![name], |row| row.get(0))
        .optional()?;
    let Some(ron_text) = data else { return Ok(None) };
    match ron::from_str::<PlayerCharacter>(&ron_text) {
        Ok(mut save) => {
            save.migrate();
            Ok(Some(save))
        }
        Err(e) => {
            eprintln!("[server] failed to parse save data for character '{name}': {e} -- treating as no save");
            Ok(None)
        }
    }
}

/// Every character on `account_id` for this `server_id`, as
/// character-select rows -- the blob is parsed only far enough to pull
/// the level and main profession out; a row whose blob won't parse is
/// logged and skipped rather than failing the whole list.
pub fn list_characters(db: &SaveDb, account_id: i64, server_id: i64) -> rusqlite::Result<Vec<CharacterSummary>> {
    let conn = db.conn();
    let mut stmt =
        conn.prepare("SELECT name, data FROM characters WHERE account_id = ?1 AND server_id = ?2 ORDER BY updated_at DESC")?;
    let rows = stmt.query_map(params![account_id, server_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (name, ron_text) = row?;
        match ron::from_str::<PlayerCharacter>(&ron_text) {
            Ok(mut save) => {
                save.migrate();
                out.push(CharacterSummary {
                    name,
                    level: save.character_level.level,
                    main_profession: save.classes.main.profession.clone(),
                });
            }
            Err(e) => eprintln!("[server] character '{name}' has an unreadable save blob ({e}) -- omitted from the list"),
        }
    }
    Ok(out)
}

/// Case-insensitively, is there already a character with this name on
/// this server? The `characters.name` column is globally `UNIQUE`
/// (server_id is always 1 for now), so the `server_id` clause is
/// forward-compatibility for real multi-server, and `COLLATE NOCASE`
/// stops "Bob" and "bob" both being handed out in the normal path
/// (`create_character`'s own `UNIQUE` catch covers the TOCTOU race).
pub fn character_name_taken(db: &SaveDb, name: &str, server_id: i64) -> rusqlite::Result<bool> {
    let found = db
        .conn()
        .query_row(
            "SELECT 1 FROM characters WHERE name = ?1 COLLATE NOCASE AND server_id = ?2",
            params![name, server_id],
            |_| Ok(()),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Does a character with exactly this name exist and belong to
/// `account_id` on this server? Used to gate `SelectCharacter` -- a
/// client can't enter the world as someone else's character even if it
/// knows the name.
pub fn character_owned_by(db: &SaveDb, name: &str, account_id: i64, server_id: i64) -> rusqlite::Result<bool> {
    let found = db
        .conn()
        .query_row(
            "SELECT 1 FROM characters WHERE name = ?1 AND account_id = ?2 AND server_id = ?3",
            params![name, account_id, server_id],
            |_| Ok(()),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Inserts a brand-new character row with `account_id`/`server_id` set
/// (unlike `UPSERT_SQL`, whose `ON CONFLICT` path deliberately leaves both
/// columns alone so autosave/logout never touch them). `Ok(false)` if the
/// `name` `UNIQUE` constraint trips, so a race between two accounts
/// creating the same name at once just loses cleanly for one of them.
pub fn create_character(
    db: &SaveDb,
    name: &str,
    account_id: i64,
    server_id: i64,
    save: &PlayerCharacter,
) -> rusqlite::Result<bool> {
    let ron_text = ron::to_string(save).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    match db.conn().execute(
        "INSERT INTO characters (name, account_id, server_id, data) VALUES (?1, ?2, ?3, ?4)",
        params![name, account_id, server_id, ron_text],
    ) {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => Ok(false),
        Err(e) => Err(e),
    }
}

/// Creates the row if `name` has never been saved before, otherwise
/// overwrites it wholesale -- a save is always the character's complete
/// current state, never a partial patch.
const UPSERT_SQL: &str = "INSERT INTO characters (name, data, updated_at) VALUES (?1, ?2, datetime('now'))
     ON CONFLICT(name) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at";

/// How long the writer waits between attempts while saves keep failing.
const SAVE_RETRY_EVERY: Duration = Duration::from_secs(2);
/// How long `SaveQueue::flush` blocks before giving up.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

enum SaveJob {
    Save { name: String, save: PlayerCharacter },
    /// Answered `true` once everything queued before it is on disk, `false`
    /// if writing is currently failing.
    Flush(Sender<bool>),
}

/// Hands saves to the background writer thread (see this module's doc).
/// `save` never blocks; the writer keeps each character's newest save until
/// it's safely written, retrying every `SAVE_RETRY_EVERY` if the database
/// is failing (locked by another program, disk full, ...).
#[derive(Resource)]
pub struct SaveQueue(Sender<SaveJob>);

impl SaveQueue {
    /// Startup only -- like `SaveDb::open`, a file that can't be opened is fatal.
    pub fn spawn(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let mut conn =
            open_connection(path).unwrap_or_else(|e| panic!("failed to open save database {path:?} for the save writer: {e}"));
        Self::spawn_with(SAVE_RETRY_EVERY, move |pending| write_saves(&mut conn, pending).map_err(|e| e.to_string()))
    }

    /// `spawn` with the database write swapped out -- tests use it to see
    /// exactly what gets saved.
    pub(crate) fn spawn_with(
        retry_every: Duration,
        write: impl FnMut(&mut HashMap<String, PlayerCharacter>) -> Result<(), String> + Send + 'static,
    ) -> Self {
        let (jobs, inbox) = channel();
        std::thread::Builder::new()
            .name("save-writer".into())
            .spawn(move || run_writer(inbox, retry_every, write))
            .expect("failed to start the save writer thread");
        Self(jobs)
    }

    pub fn save(&self, name: &str, save: PlayerCharacter) {
        if self.0.send(SaveJob::Save { name: name.to_owned(), save }).is_err() {
            eprintln!("[save] the save writer has stopped -- '{name}' was not saved");
        }
    }

    /// Blocks until everything queued so far is on disk (`true`), or writing
    /// is failing or takes longer than `FLUSH_TIMEOUT` (`false`). For the
    /// rare moments that read a character back right after it may have been
    /// saved -- selecting it again right after logging out.
    pub fn flush(&self) -> bool {
        self.flush_within(FLUSH_TIMEOUT)
    }

    /// `flush`, waiting up to `timeout` -- shutting down can afford to wait
    /// longer than a frame should.
    pub fn flush_within(&self, timeout: Duration) -> bool {
        let (reply, answer) = channel();
        self.0.send(SaveJob::Flush(reply)).is_ok() && answer.recv_timeout(timeout).unwrap_or(false)
    }
}

/// The writer thread's loop: collect everything queued (newest save per
/// character wins), write it, answer any flushes. Saves that fail stay in
/// `pending` and are retried, so a logout save -- whose character is
/// already gone from the world -- is never silently lost.
fn run_writer(
    jobs: Receiver<SaveJob>,
    retry_every: Duration,
    mut write: impl FnMut(&mut HashMap<String, PlayerCharacter>) -> Result<(), String>,
) {
    let mut pending: HashMap<String, PlayerCharacter> = HashMap::new();
    let mut failing = false;
    loop {
        // Nothing pending: sleep until work arrives. Holding unwritten
        // saves: also wake up after `retry_every` to try again.
        let first = if pending.is_empty() {
            match jobs.recv() {
                Ok(job) => Some(job),
                Err(_) => return,
            }
        } else {
            match jobs.recv_timeout(retry_every) {
                Ok(job) => Some(job),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    if let Err(e) = write(&mut pending) {
                        eprintln!("[save] shutting down with {} unsaved character(s): {e}", pending.len());
                    }
                    return;
                }
            }
        };

        let mut flushes = Vec::new();
        for job in first.into_iter().chain(jobs.try_iter()) {
            match job {
                SaveJob::Save { name, save } => {
                    pending.insert(name, save);
                }
                SaveJob::Flush(reply) => flushes.push(reply),
            }
        }

        if !pending.is_empty() {
            match write(&mut pending) {
                Ok(()) => {
                    if failing {
                        eprintln!("[save] saving works again -- {} character(s) written", pending.len());
                        failing = false;
                    }
                    pending.clear();
                }
                Err(e) => {
                    if !failing {
                        eprintln!(
                            "[save] couldn't write {} character(s), retrying every {}s: {e}",
                            pending.len(),
                            retry_every.as_secs_f32()
                        );
                        failing = true;
                    }
                }
            }
        }
        for reply in flushes {
            let _ = reply.send(pending.is_empty());
        }
    }
}

/// Writes every pending save in one transaction. A save that can't even be
/// serialized is dropped (and logged) -- retrying it could never succeed,
/// and it mustn't hold back everyone else's.
fn write_saves(conn: &mut Connection, pending: &mut HashMap<String, PlayerCharacter>) -> rusqlite::Result<()> {
    let mut rows = Vec::with_capacity(pending.len());
    pending.retain(|name, save| match ron::to_string(save) {
        Ok(data) => {
            rows.push((name.clone(), data));
            true
        }
        Err(e) => {
            eprintln!("[save] '{name}' can't be serialized, dropping this save: {e}");
            false
        }
    });
    let tx = conn.transaction()?;
    for (name, data) in &rows {
        tx.execute(UPSERT_SQL, params![name, data])?;
    }
    tx.commit()
}

pub struct PersistencePlugin;

impl Plugin for PersistencePlugin {
    fn build(&self, app: &mut App) {
        let path = std::env::var("ARPG_SAVE_DB_PATH").unwrap_or_else(|_| DEFAULT_SAVE_DB_PATH.to_string());
        println!("[server] opening save database at {path}");
        // `SaveDb` first: it creates the table the writer's connection uses.
        app.insert_resource(SaveDb::open(&path));
        app.insert_resource(SaveQueue::spawn(&path));
        app.add_systems(Update, tick_autosave);
    }
}

/// How often every named, connected character's current state is written
/// back to disk -- a graceful disconnect (`server::net`'s own hook) saves
/// immediately on top of this, so this interval only bounds how much a
/// hard crash could lose.
const AUTOSAVE_INTERVAL_SECS: f32 = 60.0;

/// `Local<Option<Timer>>`, lazily created on first run (this project has
/// no existing resource that tracks plain elapsed real time in a form
/// usable for a periodic gate like this -- `ServerTick` is a tick *count*,
/// `GameClock` is an in-game day/night clock that wraps every 24 in-game hours).
fn tick_autosave(saves: Res<SaveQueue>, time: Res<Time>, mut timer: Local<Option<Timer>>, characters: Query<SavedCharacter>) {
    let timer = timer.get_or_insert_with(|| Timer::from_seconds(AUTOSAVE_INTERVAL_SECS, TimerMode::Repeating));
    timer.tick(time.delta());
    if !timer.just_finished() {
        return;
    }
    for character in &characters {
        saves.save(&character.name.0, character.to_save());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game_core::components::{CharacterRace, Sex};
    use game_core::states::TOWN_INSTANCE;

    fn sample_save(alive: bool) -> PlayerCharacter {
        PlayerCharacter {
            position: Position(Vec2::ZERO),
            level: Level::default(),
            instance: TOWN_INSTANCE,
            race: CharacterRace("human".to_string()),
            sex: Sex::Male,
            classes: Classes::new("scholar"),
            character_level: CharacterLevel::default(),
            profession_points: ProfessionPoints::default(),
            known_abilities: KnownAbilities::default(),
            equipment: Equipment::default(),
            backpack: Backpack::new(),
            alive,
        }
    }

    /// A blob written before `alive` existed has no such key in its RON
    /// at all -- `#[serde(default = "default_alive")]` is what keeps
    /// every character saved under the old schema loading (as alive,
    /// which every one of them genuinely was) instead of erroring out of
    /// `load_character` the instant this field shipped.
    #[test]
    fn old_shape_blob_without_alive_still_loads_as_alive() {
        let ron_text = ron::to_string(&sample_save(true)).expect("serialize");
        // Strip the `alive` field back out, whichever exact spacing/comma
        // placement `ron::to_string` happened to use (it's the last
        // field, so the comma more likely precedes it than follows).
        let without_alive = ron_text
            .replacen(",alive:true", "", 1)
            .replacen(", alive: true", "", 1)
            .replacen("alive:true,", "", 1)
            .replacen("alive: true,", "", 1);
        assert!(
            !without_alive.contains("alive"),
            "test setup failed to strip the field, so this wouldn't actually exercise the default: {without_alive}"
        );
        let loaded: PlayerCharacter = ron::from_str(&without_alive).expect("an old-shape blob must still parse");
        assert!(loaded.alive);
    }

    #[test]
    fn alive_false_round_trips() {
        let ron_text = ron::to_string(&sample_save(false)).expect("serialize");
        let loaded: PlayerCharacter = ron::from_str(&ron_text).expect("deserialize");
        assert!(!loaded.alive);
    }

    /// Exercises the real DB-level backstop `SaveDb::open` sets up
    /// (`characters_name_nocase_idx`) -- not just the application-level
    /// `character_name_taken` pre-check `character_select` normally relies
    /// on, but the constraint that still holds even if that pre-check were
    /// ever bypassed or raced.
    #[test]
    fn duplicate_name_rejected_case_insensitively_at_the_db_level() {
        let file = TempDbFile::new("duplicate_name");
        let db = SaveDb::open(&file.0);
        let save = sample_save(true);

        assert!(create_character(&db, "Bob", 1, 1, &save).unwrap(), "first create should succeed");
        assert!(!create_character(&db, "bob", 2, 1, &save).unwrap(), "a case-variant duplicate must be rejected");
        assert!(!create_character(&db, "BOB", 3, 1, &save).unwrap(), "and any other casing too");
        assert!(create_character(&db, "Alice", 1, 1, &save).unwrap(), "an unrelated name still succeeds");
    }

    #[test]
    fn queued_saves_reach_the_database_and_flush_waits_for_them() {
        let file = TempDbFile::new("save_queue");
        let db = SaveDb::open(&file.0);
        let saves = SaveQueue::spawn(&file.0);

        let mut older = sample_save(true);
        older.character_level.level = 3;
        let mut newest = sample_save(true);
        newest.character_level.level = 7;
        saves.save("Bob", older);
        saves.save("Bob", newest);

        assert!(saves.flush(), "nothing is failing, so flush must report everything written");
        let loaded = load_character(&db, "Bob").unwrap().expect("Bob was saved");
        assert_eq!(loaded.character_level.level, 7, "the newest save wins");
    }

    #[test]
    fn failed_saves_are_kept_and_retried_until_they_succeed() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let written = Arc::new(Mutex::new(Vec::<String>::new()));
        let failing = Arc::new(AtomicBool::new(true));
        let saves = {
            let (written, failing) = (written.clone(), failing.clone());
            SaveQueue::spawn_with(Duration::from_millis(10), move |pending| {
                if failing.load(Ordering::SeqCst) {
                    return Err("database is locked".to_string());
                }
                written.lock().unwrap().extend(pending.keys().cloned());
                Ok(())
            })
        };

        saves.save("Bob", sample_save(true));
        assert!(!saves.flush(), "while writes fail, flush must not claim the save is on disk");

        failing.store(false, Ordering::SeqCst);
        // No new save needed -- the writer retries on its own.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while written.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*written.lock().unwrap(), vec!["Bob".to_string()], "written exactly once, after recovering");
        assert!(saves.flush());
    }

    /// A fresh temp database path; the file and its WAL side files are
    /// removed before use and (best effort) afterwards.
    struct TempDbFile(std::path::PathBuf);

    impl TempDbFile {
        fn new(label: &str) -> Self {
            let file = Self(std::env::temp_dir().join(format!("arpg_test_{label}_{}.db", std::process::id())));
            file.remove();
            file
        }

        fn remove(&self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    impl Drop for TempDbFile {
        fn drop(&mut self) {
            self.remove();
        }
    }
}
