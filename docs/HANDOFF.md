# Handoff: building game mechanics for the MMORPG

Read this first in a new session. It covers what the game is for, how it's
built, how to add a mechanic without breaking the online model, what's known
to be missing, and how to verify work. Written 2026-09-26, branch
`tibia-mapp`.

## The goal

An MMORPG where **every player plays on the same server**. `game_server` and
`auth_server` run on a host; each player installs `game_client` and connects
over the internet. Judge every change against that:

- **The server decides everything.** Clients send intent (keys held, requests),
  never outcomes. The client predicts its own movement and combat for
  responsiveness, but the server's result always wins.
- **One world, many players.** A mechanic must still work, and stay cheap in
  CPU and bandwidth, with many players in view of each other.
- **A modified client can send anything.** Validate range, floor, ownership,
  cooldowns and costs on the server.

## State right now

- Everything since commit `4ca8ea3` is **uncommitted** (about 100 changed
  files). Commit before starting new work, and only when the user asks.
- An architecture audit is fully worked through: shared codec and 30 Hz
  snapshots, render interpolation, schedule phases, terrain chunk meshes,
  module split, debug-tool gating, Docker, graceful shutdown.
- Build: `cargo build --workspace`, no warnings. Tests: `cargo test --workspace`,
  67 passing.
- Login phases 1–4 are done (accounts, character select, saving). Phase 5
  (hardening) is not; see "Known gaps".

## Run and test

```bash
cargo dev                 # auth + game server + one client window (xtask)
cargo dev --clients 2     # two windows, log in with two accounts
cargo dev --no-client     # servers only
cargo test --workspace
docker compose up --build -d   # hosted stack: auth 5001/tcp, game 5000/udp
```

- The client starts at a login screen. Register any email and a password of
  8+ characters, create a character, and you spawn in Rookgaard next to Lucas
  the NPC.
- Dev keys (with the default `debug-tools` feature): H collision overlay,
  L light radius, F5 level up, a teleport button. The server only honours
  level-up and teleport with `ARPG_DEBUG_COMMANDS=1`; `cargo dev` sets it,
  Docker doesn't.
- F3: frame rate and worst frame time (every build). The server logs frames
  over its 16.7 ms budget. For per-system timings: `--features bevy/trace_tracy`.
- Player build: `cargo build --release -p game_client --no-default-features`.
- Settings: `config/gameplay.ron` (shared tuning), `config/input.ron` (default
  keys; players' own changes go to `%APPDATA%\arpg-skeleton\keybinds.ron`),
  `data/*.ron` (content), `gallery/maps/` (world and zones). Secrets live in
  `.env` (`ARPG_GROQ_API_KEY`); never print it.

## Architecture in one page

**Crates.** `core` (`game_core`) is the simulation, shared by client and
server; it has no rendering. `protocol` holds the wire messages and their
codec. `server` is authoritative and headless. `client` is Bevy with
rendering. `auth_server` is HTTP accounts and sessions. `map_generator` is a
content tool. `xtask` is the `cargo dev` runner.

**One tick** (`FixedUpdate`, 60 Hz, identical on client and server) runs these
phases in order (`core/src/schedule.rs`, registered in `core/src/lib.rs`):
Input → Clock → Ai → Intent → Movement → Collision → Floors → Timers →
Actions → Resolve → Progression. Systems inside each phase are chained. A
core test fails if two sim systems touch the same data without an order
between them.

**Networking** (renet over UDP):
- **Client → server.** `ClientInput` every tick on the unreliable channel
  (the server applies one per sim step). `ClientMessage` requests go on the
  reliable channel; the server decodes them into `ClientRequest` events,
  which feature systems handle in `RequestSet::Handle` (logout in
  `RequestSet::Leave`).
- **Server → client, per tick.** `ServerMessage::Snapshot` every 2 ticks
  (30 Hz). It holds only what that player can see: vision radius, walls
  (line of sight), floors (the one below shows through gaps; the one above
  only via lights), and lights (orbs, fire tiles).
- **Server → client, on change.** Reliable `ServerMessage`s such as
  backpack, equipment, abilities, progression and chat.
- **Wire format.** Encode and decode only through `protocol::encode`/`decode`
  (varint bincode, 256 KiB cap). Data-file names travel as `NameId` (u16),
  with the table sent once in `SnapshotSetup`. **Bump `PROTOCOL_ID` whenever
  the wire format changes** (it's 3 now).

**Client smoothing.** The local player is predicted, then reconciled against
the snapshot's `your_last_processed_input_tick` (`client/src/reconciliation.rs`).
Remote entities are drawn about 3 snapshot intervals behind, interpolated
(`client/src/interpolation.rs`). Anything drawn at an entity reads
`RenderPosition` and runs in `DrawSet`.

**Persistence.** One SQLite row per character (`saves/game.db`), holding a
RON blob of `core::player::PlayerCharacter`. Saved every 60 s, on logout, on
disconnect, and on server stop (Ctrl+C or SIGTERM saves everyone first).
Writes go through a background thread (`SaveQueue`). Accounts live in
`saves/auth.db`, owned only by `auth_server`. A session token rides the
connection handshake, and the server validates it over HTTP on a worker
thread. Leaving is Tibia-style: logout needs 10 s out of combat and no
aggro; a raw disconnect leaves an `Abandoned` body in the world.

## Where things live

| Area | Path |
|---|---|
| Tick phases, system registration | `core/src/schedule.rs`, `core/src/lib.rs` |
| Components (re-exported as `game_core::components::X`) | `core/src/components/{body,input,combat,abilities,character,items,ai}.rs` |
| Combat (timers → resolution → attacks/abilities → hits → projectiles) | `core/src/systems/combat/` |
| Movement, collision, floors, AI, XP/stats | `core/src/systems/*.rs` |
| Content schemas and registries (RON in `data/`) | `core/src/{ability,item,creature,npc,race,profession,stats,damage,*_defense}.rs` |
| Maps (tiles, autotile, zones, stitched world, LOS geometry) | `core/src/map/` |
| What a character saves | `core/src/player.rs` (`PlayerCharacter`, `PlayerSimBundle`) |
| Wire messages and codec | `protocol/src/lib.rs` |
| Server: connections, inputs, snapshots and visibility | `server/src/net.rs` |
| Server features | `character_select`, `persistence`, `loot`, `equip`, `profession_requests`, `light_orb`, `npc_dialogue`, `chat`, `logout`, `map` (spawns), `shutdown`, `frame_budget`, `config` (`DebugCommands`) |
| Client plugin groups (table of contents) | `client/src/plugins.rs`: `NetPlugins`, `WorldPlugins`, `UiPlugins`; `debug::DebugPlugins` |
| Client networking | `client/src/net/` (`messages`, `input`, `snapshots`) |
| Client rendering | `map.rs` + `tile_chunks.rs` (terrain meshes), `floor_display.rs`, `floor_shade.rs`, `vision.rs`, `animation/`, `*_display.rs` |
| Client windows and UI | `ui.rs`, `hud.rs`, `minimap.rs`, `abilities_ui.rs`, `character_stats_ui.rs`, `loot_ui.rs`, `item_*.rs`, `chat_ui.rs`, `logout_ui.rs`, `death_screen.rs`, `disconnect_screen.rs`, `login_ui.rs`, `character_select_ui.rs` |
| Key bindings | `client/src/config.rs` (`InputConfig`, `ReservedKeys`, player keybinds file) |

How-to guides already written: `docs/adding-a-creature.md`,
`docs/adding-a-zone.md`, `docs/adding-an-ability.md`,
`docs/damage-and-defense.md`, `docs/npc-ai-dialogue-system.md`. `readme.md`
is in Spanish, for the user.

## Adding a game mechanic: checklist

1. **Decide where it runs.**
   - **Shared `core` system**, if the player must feel it instantly and it's
     deterministic (movement-like, combat timing). Put it in the right
     `SimSet` chain in `core/src/lib.rs`.
   - **Server-only**, if it's random, secret or economic: loot rolls, XP,
     trades, anything the client mustn't decide. Clients learn the result
     from a snapshot or a message.
   - **Client-only**, if it's pure presentation.
2. **Data.** Add the schema in `core`, content in `data/*.ron`, and load it
   in both `server/src/data.rs` and `client/src/data.rs` (both read the same
   files).
3. **Components.** Put them in the matching `core/src/components/<area>.rs`.
   If players must keep it, add a field to `PlayerCharacter` with
   `#[serde(default)]` so old saves still load.
4. **Input.**
   - **Continuous or held.** Add a `ClientInput` field, applied in
     `server/src/net.rs` (`apply_client_inputs`) and read in
     `client/src/net/input.rs`. Add a `PlayerAction` plus a default key in
     `config/input.ron`.
   - **Discrete request.** Add a `ClientMessage` variant, handled by a server
     system reading `ClientRequest` in `RequestSet::Handle`. Validate
     everything there (see "The goal").
   - **Keys the client reads directly** (not via `PlayerAction`) must call
     `app.reserve_key(...)`.
5. **Output.**
   - **Per-tick visible state.** Add a field on `EntitySnapshot`. Keep it
     tiny (about 50 bytes per entity today); strings go as `NameId`.
   - **Everything else.** A reliable `ServerMessage`, handled on the client
     from `FromServer` events in `HandleServerMessages`.
   - Anything visible must respect the per-player visibility in
     `broadcast_snapshots`.
6. **Bump `PROTOCOL_ID`** if you touched the wire format.
7. **UI.** A client plugin, added to the right group in `client/src/plugins.rs`.
   Dev-only tools go in `client/src/debug/`, and their server side goes
   behind `DebugCommands`.
8. **Verify** (next section), then describe to the user what to test in
   game.

## Verifying work

The user tests in game themselves. **Never drive their mouse or keyboard.**
Instead:

- `cargo build --workspace` with no warnings, and `cargo test --workspace`.
- **After any client change, boot the real client** for about 15 s (spawn
  `target/debug/game_client.exe`, check it's still running with no
  `panicked`). Unit tests missed a startup crash once. Check the
  `--no-default-features` build too.
- **Server end to end.** Write temporary `server/examples/*.rs` renet
  clients: register via `http://127.0.0.1:5001/register`, create or select a
  character, read snapshots. Run servers on temp DBs (`ARPG_AUTH_DB_PATH`,
  `ARPG_SAVE_DB_PATH`) from one script. Delete the examples afterwards.
- **Rendering or UI.** Build a temporary env-var-gated harness app that
  renders just the piece and saves a screenshot; diff before and after with
  PIL.
- **Docker.** Same test clients against the compose stack, then
  `docker compose down -v`. Never run `docker compose config` without
  `--quiet`: it would print the key from `.env`.

## Scaling to "everyone on one server": current limits

- **`max_clients: 32`** in `server/src/net.rs`. Raise it before any real
  test with many players.
- **One process, one world, everyone in `TOWN_INSTANCE`.** `InstanceId` only
  filters today; nothing creates instances.
- **Visibility is per player × per entity**, with line-of-sight checks
  (`broadcast_snapshots`). Fine now; crowds will need a spatial grid (interest
  management).
- **Snapshots send full state** 30 times a second. The next saving is
  quantized positions and deltas, plus not sending the charge and aim fields
  (16 bytes per entity) when not charging.
- **Netcode runs in unsecure mode**, with the session token in `user_data`.
  Phase 5: secure connect tokens (the server will need its public address),
  and TLS for auth via a reverse proxy.
- **Chat is proximity-only.** No party, guild or global channels.
- **Creature AI** only runs near players (`creature_activity_radius`).
- **Measure before optimizing:** F3, the server frame-budget log, Tracy.

## Known gaps and half-built hooks (good next mechanics)

- **Player names.** Chat shows `Player<id>` (`server/src/chat.rs`,
  `placeholder_display_name`), and there are no nameplates. Characters do
  have names (`CharacterName`, server-side), which just need sending.
- **Status effects** are inserted on hit (`StatusEffect`), but nothing reads
  them yet: poison, slow, stun and so on.
- **No item-use system.** `ItemEffect` (for example `IncreaseLightRadius`)
  is never applied. `SwapProfessionItem` is a placeholder.
- **Missing social systems:** instances or dungeons (the filter hook exists),
  parties, player-to-player trade, guilds, PvP rules.
- **NPCs.** Only Lucas exists. He trades via chat using an LLM, with prices
  enforced from data, and has no persistent memory of players.
  See `docs/npc-ai-dialogue-system.md`.
- **Races.** New characters are always human. The client assumes the
  default race, so send it (for example in `Welcome`) if races become
  selectable.
- **Phase 5 hardening:** TLS, hashing session tokens at rest, email
  verification, lockout and reset, DB backups. Also a case-sensitivity race
  on character names.
- **Small issues:**
  - A Rookgaard chest references a missing item, `leather_tunic`.
  - Light tiles on upper floors don't light that floor.
  - An orb you carry is drawn slightly behind you.
  - Remote players walking under a bridge pause at its edge, then fade.
  - Closing the game window in `cargo dev` stops the servers by force (Ctrl+C
    saves first).

## Working agreements with the user

- **Ask before adding a dependency.** Commit or push only when asked.
- **Code style.** Doc comments explain *why*, at the density of the
  surrounding code. Imports are explicit, grouped std / external /
  `game_core`+`protocol` / `crate` / `super`. The codebase is **not**
  rustfmt-formatted, so don't format whole files.
- **Re-exports.** Paths like `game_core::components::X` and
  `game_core::systems::combat::X` are re-exports and stay valid. Doc
  comments across the code use them.
- **Communication.** Explain in plain terms what changed and what to test in
  game. The user writes in English, sometimes Spanish; the README is Spanish.
