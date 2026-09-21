//! Character save/load -- SQLite-backed (via `rusqlite`, blocking: this
//! project has no async runtime anywhere, and SQLite operations for a
//! handful of players are microseconds, well inside the 16.6ms/tick
//! budget even called directly from a system), one row per character, a
//! single RON-serialized blob column rather than a fully normalized
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
//! / `upsert_character` (autosave and logout only ever know a name), and
//! `name` is still globally `UNIQUE` -- `server_id` is always `1` until
//! real multi-server exists.

use std::path::Path;
use std::sync::Mutex;

use bevy::prelude::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use protocol::CharacterSummary;

use game_core::components::{
    Backpack, CharacterLevel, CharacterRace, Classes, Equipment, KnownAbilities, Level, Position, ProfessionPoints,
    Sex, SpellPoints,
};
use game_core::states::{CombatState, InstanceId};

/// Where `SaveDb::open` puts the SQLite file by default -- overridable
/// via `ARPG_SAVE_DB_PATH`, same env-var-with-a-default idiom every other
/// data path in this project already uses (`ARPG_GAMEPLAY_CONFIG_PATH`
/// etc.).
pub const DEFAULT_SAVE_DB_PATH: &str = "saves/game.db";

/// Everything about a character that survives a disconnect. Composed
/// directly from the real component types (all already `Serialize`/
/// `Deserialize` -- they're all sent over the wire via `protocol`
/// messages too) rather than a shadow struct with its own duplicated
/// field list, so there's nothing to keep manually in sync as components
/// themselves change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterSave {
    pub position: Position,
    pub level: Level,
    pub instance: InstanceId,
    pub race: CharacterRace,
    pub sex: Sex,
    pub classes: Classes,
    pub character_level: CharacterLevel,
    pub profession_points: ProfessionPoints,
    pub spell_points: SpellPoints,
    pub known_abilities: KnownAbilities,
    pub equipment: Equipment,
    pub backpack: Backpack,
    /// Whether this character was alive (not `states::CombatState::Dead`)
    /// at the moment this was saved. `Health`/`CombatState` are otherwise
    /// never persisted at all -- `character_select::spawn_player_entity`
    /// always recomputes `Health` fresh from the race on spawn -- but
    /// death itself has to survive a save/load, or a character that died
    /// while its owner was disconnected (see `server::logout`'s own
    /// `Abandoned`/sweep doc) would just come back alive next login,
    /// silently undoing its own death. `#[serde(default = "default_alive")]`
    /// so every character saved before this field existed keeps loading
    /// (as alive, which every one of them in fact was).
    #[serde(default = "default_alive")]
    pub alive: bool,
}

fn default_alive() -> bool {
    true
}

/// Builds a `CharacterSave` from a live entity's own current component
/// values -- shared by every call site that needs to persist "whatever
/// this character's state is right now" (the autosave tick below,
/// `server::net`'s disconnect handler, `server::logout::
/// sweep_abandoned_characters`, and `server::loot`'s own `EnterWorldReady`/
/// `LogoutRequest` handling), so the field-by-field `.clone()` list
/// exists exactly once instead of drifting across several hand-copied
/// versions.
#[allow(clippy::too_many_arguments)]
pub fn save_from_components(
    position: &Position,
    level: &Level,
    instance: &InstanceId,
    race: &CharacterRace,
    sex: &Sex,
    classes: &Classes,
    character_level: &CharacterLevel,
    profession_points: &ProfessionPoints,
    spell_points: &SpellPoints,
    known_abilities: &KnownAbilities,
    equipment: &Equipment,
    backpack: &Backpack,
    combat_state: &CombatState,
) -> CharacterSave {
    CharacterSave {
        position: position.clone(),
        level: level.clone(),
        instance: instance.clone(),
        race: race.clone(),
        sex: sex.clone(),
        classes: classes.clone(),
        character_level: character_level.clone(),
        profession_points: profession_points.clone(),
        spell_points: spell_points.clone(),
        known_abilities: known_abilities.clone(),
        equipment: equipment.clone(),
        backpack: backpack.clone(),
        alive: !matches!(combat_state, CombatState::Dead),
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

impl SaveDb {
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create saves directory");
        }
        let conn = Connection::open(path).unwrap_or_else(|e| panic!("failed to open save database {path:?}: {e}"));
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
}

/// `None` if no character with this name has ever been saved -- the
/// caller's job to decide that means "create one," not this function's.
pub fn load_character(db: &SaveDb, name: &str) -> Option<CharacterSave> {
    let conn = db.0.lock().expect("save database mutex poisoned");
    let data: Option<String> = conn
        .query_row("SELECT data FROM characters WHERE name = ?1", params![name], |row| row.get(0))
        .optional()
        .expect("failed to query character save");
    let ron_text = data?;
    match ron::from_str(&ron_text) {
        Ok(save) => Some(save),
        Err(e) => {
            eprintln!("[server] failed to parse save data for character '{name}': {e} -- treating as no save");
            None
        }
    }
}

/// Creates the row if `name` has never been saved before, otherwise
/// overwrites it wholesale -- a save is always the character's complete
/// current state, never a partial patch.
pub fn upsert_character(db: &SaveDb, name: &str, save: &CharacterSave) {
    let ron_text = ron::to_string(save).expect("failed to serialize character save");
    let conn = db.0.lock().expect("save database mutex poisoned");
    conn.execute(
        "INSERT INTO characters (name, data, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(name) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at",
        params![name, ron_text],
    )
    .expect("failed to upsert character save");
}

/// Every character on `account_id` for this `server_id`, as
/// character-select rows -- the blob is parsed only far enough to pull
/// the level and main profession out; a row whose blob won't parse is
/// logged and skipped rather than failing the whole list.
pub fn list_characters(db: &SaveDb, account_id: i64, server_id: i64) -> Vec<CharacterSummary> {
    let conn = db.0.lock().expect("save database mutex poisoned");
    let mut stmt = conn
        .prepare("SELECT name, data FROM characters WHERE account_id = ?1 AND server_id = ?2 ORDER BY updated_at DESC")
        .expect("failed to prepare character list query");
    let rows = stmt
        .query_map(params![account_id, server_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("failed to query character list");
    let mut out = Vec::new();
    for row in rows {
        let (name, ron_text) = row.expect("failed to read character list row");
        match ron::from_str::<CharacterSave>(&ron_text) {
            Ok(save) => out.push(CharacterSummary {
                name,
                level: save.character_level.level,
                main_profession: save.classes.main.profession.clone(),
            }),
            Err(e) => eprintln!("[server] character '{name}' has an unreadable save blob ({e}) -- omitted from the list"),
        }
    }
    out
}

/// Case-insensitively, is there already a character with this name on
/// this server? The `characters.name` column is globally `UNIQUE`
/// (server_id is always 1 for now), so the `server_id` clause is
/// forward-compatibility for real multi-server, and `COLLATE NOCASE`
/// stops "Bob" and "bob" both being handed out in the normal path
/// (`create_character`'s own `UNIQUE` catch covers the TOCTOU race).
pub fn character_name_taken(db: &SaveDb, name: &str, server_id: i64) -> bool {
    let conn = db.0.lock().expect("save database mutex poisoned");
    conn.query_row(
        "SELECT 1 FROM characters WHERE name = ?1 COLLATE NOCASE AND server_id = ?2",
        params![name, server_id],
        |_| Ok(()),
    )
    .optional()
    .expect("failed to query character name")
    .is_some()
}

/// Does a character with exactly this name exist and belong to
/// `account_id` on this server? Used to gate `SelectCharacter` -- a
/// client can't enter the world as someone else's character even if it
/// knows the name.
pub fn character_owned_by(db: &SaveDb, name: &str, account_id: i64, server_id: i64) -> bool {
    let conn = db.0.lock().expect("save database mutex poisoned");
    conn.query_row(
        "SELECT 1 FROM characters WHERE name = ?1 AND account_id = ?2 AND server_id = ?3",
        params![name, account_id, server_id],
        |_| Ok(()),
    )
    .optional()
    .expect("failed to query character ownership")
    .is_some()
}

/// Inserts a brand-new character row with `account_id`/`server_id` set
/// (unlike `upsert_character`, whose `ON CONFLICT` path deliberately
/// leaves both columns alone so autosave/logout never touch them).
/// Returns `false` -- not a panic -- if the `name` `UNIQUE` constraint
/// trips, so a race between two accounts creating the same name at once
/// just loses cleanly for one of them.
pub fn create_character(db: &SaveDb, name: &str, account_id: i64, server_id: i64, save: &CharacterSave) -> bool {
    let ron_text = ron::to_string(save).expect("failed to serialize character save");
    let conn = db.0.lock().expect("save database mutex poisoned");
    match conn.execute(
        "INSERT INTO characters (name, account_id, server_id, data) VALUES (?1, ?2, ?3, ?4)",
        params![name, account_id, server_id, ron_text],
    ) {
        Ok(_) => true,
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => false,
        Err(e) => panic!("failed to create character '{name}': {e}"),
    }
}

pub struct PersistencePlugin;

impl Plugin for PersistencePlugin {
    fn build(&self, app: &mut App) {
        let path = std::env::var("ARPG_SAVE_DB_PATH").unwrap_or_else(|_| DEFAULT_SAVE_DB_PATH.to_string());
        println!("[server] opening save database at {path}");
        app.insert_resource(SaveDb::open(&path));
        app.add_systems(Update, tick_autosave);
    }
}

/// How often every named, connected character's current state is written
/// back to disk -- a graceful disconnect (`server::net`'s own hook) saves
/// immediately on top of this, so this interval only bounds how much a
/// hard crash could lose.
const AUTOSAVE_INTERVAL_SECS: f32 = 60.0;

/// `Local<Option<Timer>>`, lazily created on first run -- same pattern
/// `client::abilities_ui::sync_window`'s own refresh timer already uses,
/// just server-side for the first time (this project has no existing
/// resource that tracks plain elapsed real time in a form usable for a
/// periodic gate like this -- `ServerTick` is a tick *count*, `GameClock`
/// is an in-game day/night clock that wraps every 24 in-game hours).
#[allow(clippy::type_complexity)]
fn tick_autosave(
    db: Res<SaveDb>,
    time: Res<Time>,
    mut timer: Local<Option<Timer>>,
    query: Query<(
        &CharacterName,
        &Position,
        &Level,
        &InstanceId,
        &CharacterRace,
        &Sex,
        &Classes,
        &CharacterLevel,
        &ProfessionPoints,
        &SpellPoints,
        &KnownAbilities,
        &Equipment,
        &Backpack,
        &CombatState,
    )>,
) {
    let timer = timer.get_or_insert_with(|| Timer::from_seconds(AUTOSAVE_INTERVAL_SECS, TimerMode::Repeating));
    timer.tick(time.delta());
    if !timer.just_finished() {
        return;
    }
    for (name, position, level, instance, race, sex, classes, character_level, profession_points, spell_points, known_abilities, equipment, backpack, combat_state) in
        &query
    {
        let save = save_from_components(
            position,
            level,
            instance,
            race,
            sex,
            classes,
            character_level,
            profession_points,
            spell_points,
            known_abilities,
            equipment,
            backpack,
            combat_state,
        );
        upsert_character(&db, &name.0, &save);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game_core::components::{CharacterRace, ProfessionProgress, Sex};
    use game_core::states::TOWN_INSTANCE;

    fn sample_save(alive: bool) -> CharacterSave {
        CharacterSave {
            position: Position(Vec2::ZERO),
            level: Level::default(),
            instance: TOWN_INSTANCE,
            race: CharacterRace("human".to_string()),
            sex: Sex::Male,
            classes: Classes { main: ProfessionProgress::new("arcanist"), secondary: Vec::new() },
            character_level: CharacterLevel::default(),
            profession_points: ProfessionPoints::default(),
            spell_points: SpellPoints(std::collections::HashMap::new()),
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
        let loaded: CharacterSave = ron::from_str(&without_alive).expect("an old-shape blob must still parse");
        assert!(loaded.alive);
    }

    #[test]
    fn alive_false_round_trips() {
        let ron_text = ron::to_string(&sample_save(false)).expect("serialize");
        let loaded: CharacterSave = ron::from_str(&ron_text).expect("deserialize");
        assert!(!loaded.alive);
    }

    /// Exercises the real DB-level backstop `SaveDb::open` sets up
    /// (`characters_name_nocase_idx`) -- not just the application-level
    /// `character_name_taken` pre-check `character_select` normally relies
    /// on, but the constraint that still holds even if that pre-check were
    /// ever bypassed or raced.
    #[test]
    fn duplicate_name_rejected_case_insensitively_at_the_db_level() {
        let path = std::env::temp_dir().join(format!("arpg_test_duplicate_name_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = SaveDb::open(&path);
        let save = sample_save(true);

        assert!(create_character(&db, "Bob", 1, 1, &save), "first create should succeed");
        assert!(!create_character(&db, "bob", 2, 1, &save), "a case-variant duplicate must be rejected");
        assert!(!create_character(&db, "BOB", 3, 1, &save), "and any other casing too");
        assert!(create_character(&db, "Alice", 1, 1, &save), "an unrelated name still succeeds");

        drop(db);
        let _ = std::fs::remove_file(&path);
    }
}
