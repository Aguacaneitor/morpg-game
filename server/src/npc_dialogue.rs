//! LLM-driven NPC dialogue and trading -- `docs/npc-ai-dialogue-system.md`
//! is the design this implements: a hierarchical system prompt (world
//! base -> region -> location -> personality -> job -> character, all
//! from `game_core::npc`) sent, along with the running conversation and
//! the player's new line, to an OpenAI-chat-completions-shaped LLM
//! endpoint. Groq today (`ARPG_GROQ_URL` defaults to its API); a local
//! Ollama server run in OpenAI-compat mode should work by just pointing
//! `ARPG_GROQ_URL` at it later, since both speak the same request/
//! response shape -- Claude does not, and would need a real second code
//! path here, not just an env var change.
//!
//! The call itself is blocking (`ureq`, real network latency to a real
//! external API -- far too slow to ever run inline in a tick) and always
//! happens on a throwaway `std::thread`, reported back over a channel --
//! the exact same shape `character_select::validate_token`/
//! `ValidationInbox` already established for the `/validate` call.
//!
//! The reply's own `transaction` field, if it resolves to an accepted
//! trade, is executed here -- against `npc::NpcDefinition::sells`/`buys`'
//! own authoritative prices, **never** whatever number the LLM itself
//! narrated. A well-behaved model will echo the prices its own system
//! prompt was given most of the time, but "most of the time" isn't a
//! standard to let anyone's actual gold balance depend on -- the exact
//! same "never trust it, no matter how well-behaved it usually is"
//! posture this project already takes with the client.
//!
//! Player memory/affinity (the `[PLAYER MEMORY & CONTEXT]` layer) has no
//! persistence at all yet: every conversation starts as a clean stranger
//! (`build_player_memory_block`), and the only "memory" that exists is
//! `NpcConversations`' own in-process, per-connection scrollback -- gone
//! the moment either side disconnects, same ephemeral treatment
//! `chat_ui::ChatHistory` already gets client-side. A real persistent-
//! affinity/RAG store is the natural next layer on top of this, not
//! built here.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy_renet::renet::{ClientId, DefaultChannel, RenetServer};

use game_core::components::{Backpack, Facing, ItemSlots, NetworkId, Npc, Position, Wander, WanderState};
use game_core::item::ItemRegistry;
use game_core::npc::{NpcDefinition, NpcId, NpcLore, NpcRegistry};
use protocol::ServerMessage;

use crate::net::Lobby;
use crate::persistence::CharacterName;

/// Refreshed onto a focused NPC's `Wander` every frame (`tick_npc_focus`)
/// for as long as someone holds his attention, so he stands still and
/// faces them for the whole conversation. Only matters as the "grace
/// period" after focus is released: he resumes wandering once this many
/// seconds run out, rather than instantly snapping away mid-goodbye.
const TALK_FACE_PAUSE_SECS: f32 = 3.0;
/// A conversation with no accepted message from the focused player for
/// this long is considered finished -- frees the NPC for whoever's
/// waiting (`NpcFocus::waiting`). Generous enough to cover typing plus
/// an LLM round-trip; the other ways a conversation ends (a farewell word,
/// walking past `LEAVE_RANGE`, disconnecting) don't wait for this.
const CONVERSATION_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Past this distance (world units) the focused player is treated as
/// having walked away. Deliberately looser than `TALK_RANGE` -- starting
/// or continuing a chat needs `TALK_RANGE`, but drifting a step outside
/// it mid-sentence shouldn't instantly hand the NPC to someone else.
const LEAVE_RANGE: f32 = game_core::npc::TALK_RANGE * 1.5;
/// A small, fixed set of greeting words that "wake up" a nearby NPC from
/// ordinary proximity chat -- see `server::chat`'s own doc for where this
/// is actually used. Deliberately
/// not "any message at all": an NPC reacting to every unrelated line two
/// players happen to exchange near it would spend a real LLM call (and
/// look bizarre) on chat that was never meant for it. Once the NPC's
/// attention is on a player (`NpcFocus::is_focused_on`), every subsequent
/// line from that same player is forwarded regardless -- only the opening
/// line needs to be a recognized greeting. A greeting from a *different*
/// player while the NPC is busy is forwarded too, but only so the NPC can
/// politely ask them to wait.
const GREETINGS: &[&str] = &["hola", "hello", "hi", "hey", "hiya", "greetings", "yo", "sup"];
/// Words that end a conversation on the spot (the NPC still gets to say
/// goodbye -- the message goes to the LLM like any other -- but frees
/// itself for whoever's waiting the moment that reply lands).
const FAREWELLS: &[&str] = &["bye", "goodbye", "adios", "adiós", "chau", "farewell", "cya", "see you", "later"];

fn normalize_chat_word(text: &str) -> String {
    text.trim().trim_end_matches(['!', '.', ',', '?']).to_ascii_lowercase()
}

/// See `GREETINGS`'s own doc. Case-insensitive, and tolerant of one
/// trailing punctuation mark (`"hello!"`, `"hi,"`) -- not a fuzzy match
/// beyond that, since the whole point is a short, predictable list a
/// player can reliably trigger on purpose.
pub fn is_greeting(text: &str) -> bool {
    GREETINGS.contains(&normalize_chat_word(text).as_str())
}

/// See `FAREWELLS`'s own doc -- same matching rules as `is_greeting`.
fn is_farewell(text: &str) -> bool {
    FAREWELLS.contains(&normalize_chat_word(text).as_str())
}

const DEFAULT_GROQ_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
/// Tested directly against the user's own Groq account while building
/// this: returns clean, unfenced JSON matching our contract on the first
/// try. Overridable (`ARPG_GROQ_MODEL`) without a code change if Groq's
/// own catalog moves on.
const DEFAULT_GROQ_MODEL: &str = "openai/gpt-oss-20b";
/// Turns kept per (connection, NPC) conversation -- one player line plus
/// one NPC reply is two. Bounds both the prompt's own growing size and
/// how much one long back-and-forth can cost.
const MAX_HISTORY_TURNS: usize = 12;
/// Minimum real time between two messages from the same player to the
/// same NPC -- a blunt, cheap guard against spamming (and paying for)
/// LLM calls, independent of anything conversational.
const MESSAGE_COOLDOWN: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Every trade in this game is priced in this item -- there's no
/// separate numeric "gold" field anywhere, `data/items.ron`'s own
/// `gold_coin` (category `Currency`) *is* the currency, same as any
/// other stackable item.
const GOLD_COIN: &str = "gold_coin";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

struct Turn {
    role: Role,
    content: String,
}

#[derive(Default)]
struct Conversation {
    history: Vec<Turn>,
    last_message_at: Option<Instant>,
}

/// One live conversation per (connection, NPC) -- see this module's own
/// doc for why this, not anything durable, is the entirety of an NPC's
/// "memory" today.
#[derive(Resource, Default)]
pub struct NpcConversations(HashMap<(ClientId, NetworkId), Conversation>);

/// Who an NPC is currently giving his full attention to.
struct Focus {
    client: ClientId,
    /// Last time this player's message was accepted (or its reply landed)
    /// -- `CONVERSATION_IDLE_TIMEOUT` counts from here.
    last_activity: Instant,
    /// Set when the player's latest line was a `FAREWELLS` word: the NPC
    /// releases focus as soon as his goodbye reply has been sent.
    ending: bool,
}

/// One-at-a-time attention per NPC. While an NPC has a `Focus` he stands
/// still facing that player (`tick_npc_focus`) and only converses with
/// them; anyone else who greets him gets a polite "please wait"
/// (`spawn_dialogue_requests`) and joins `waiting`. When the conversation
/// ends (farewell, idle timeout, walked off, disconnected) the first
/// still-present waiting player is called up next (`tick_npc_focus`).
/// Purely in-memory, like `NpcConversations` -- a server restart just
/// means everyone's free again.
#[derive(Resource, Default)]
pub struct NpcFocus {
    current: HashMap<NetworkId, Focus>,
    waiting: HashMap<NetworkId, Vec<ClientId>>,
}

impl NpcFocus {
    /// Whether `npc` is currently listening to `client` specifically --
    /// `server::chat` uses this to decide whether an ordinary chat line
    /// (not just a recognized `is_greeting`) should be forwarded to a
    /// nearby NPC as a continuation, rather than requiring the player to
    /// repeat a greeting word every single line.
    pub fn is_focused_on(&self, npc: NetworkId, client: ClientId) -> bool {
        self.current.get(&npc).is_some_and(|focus| focus.client == client)
    }

    fn is_busy_with_other(&self, npc: NetworkId, client: ClientId) -> bool {
        self.current.get(&npc).is_some_and(|focus| focus.client != client)
    }

    /// Claims (or refreshes) `npc`'s attention for `client`. Callers must
    /// have already ruled out `is_busy_with_other`.
    fn focus_on(&mut self, npc: NetworkId, client: ClientId, ending: bool) {
        self.current.insert(npc, Focus { client, last_activity: Instant::now(), ending });
        if let Some(queue) = self.waiting.get_mut(&npc) {
            queue.retain(|waiting| *waiting != client);
        }
    }

    fn enqueue(&mut self, npc: NetworkId, client: ClientId) {
        let queue = self.waiting.entry(npc).or_default();
        if !queue.contains(&client) {
            queue.push(client);
        }
    }

    /// Called when a reply for (`client`, `npc`) has just been sent: counts
    /// as activity (so LLM latency never eats into the idle timeout), and
    /// releases the NPC if that conversation was wrapping up.
    fn finish_turn(&mut self, npc: NetworkId, client: ClientId) {
        let Some(focus) = self.current.get_mut(&npc) else { return };
        if focus.client != client {
            return;
        }
        focus.last_activity = Instant::now();
        if focus.ending {
            self.current.remove(&npc);
        }
    }
}

/// What actually happened to a trade the LLM's own `transaction.status:
/// "accepted"` proposed -- computed and executed server-side against
/// `NpcDefinition::sells`/`buys`' own authoritative prices, never against
/// whatever number the LLM itself narrated (see this module's own doc).
#[derive(Clone)]
struct TradeResult {
    succeeded: bool,
    /// Short, player-facing reason either way (e.g. "Bought a Torch for
    /// 5 gold." or "You don't have enough gold for that.").
    note: String,
}

/// Sends one line of NPC speech to `client_id` as an ordinary chat line
/// (log + speech bubble over the NPC's head, via `client::chat_ui::
/// receive_chat_messages`) -- chat is the only way a player talks with an
/// NPC, so it's also the only place a reply can appear. A completed (or
/// refused) trade's outcome note is appended in brackets to the same
/// line rather than sent separately: a second line from the same sender
/// would immediately replace the first one's speech bubble, and the note
/// matters (the NPC may cheerfully say "deal!" on a trade the server then
/// refuses, e.g. a full bag). Only ever sent to the one player being
/// spoken to, not fanned out to everyone nearby the way a real player's
/// own chat line is (`server::chat::handle_chat_messages`) -- a real
/// per-listener AOI broadcast for this is future polish.
fn npc_say(
    server: &mut RenetServer,
    client_id: ClientId,
    npc: NetworkId,
    npc_name: &str,
    dialogue: String,
    trade_result: Option<TradeResult>,
) {
    let text = match trade_result {
        Some(result) => format!("{dialogue} [{}]", result.note),
        None => dialogue,
    };
    if let Ok(bytes) = bincode::serialize(&ServerMessage::ChatBroadcast { sender: npc, sender_name: npc_name.to_string(), text }) {
        server.send_message(client_id, DefaultChannel::ReliableUnordered, bytes);
    }
}

/// Lines a player said in chat that an NPC should answer -- queued by
/// `server::chat::handle_chat_messages` once it's confirmed the sender is
/// in range of, and facing, an actual `Npc` (and it's a greeting or the
/// NPC's already listening to them). Drained by `spawn_dialogue_requests`,
/// which does the rest of the validation (busy/cooldown) and is where
/// prompt assembly actually happens.
#[derive(Resource, Default)]
pub struct PendingDialogueRequests(pub Vec<(ClientId, NetworkId, NpcId, String)>);

/// `api_key: None` (`ARPG_GROQ_API_KEY` unset or empty) doesn't stop the
/// server from running -- NPC dialogue just always falls back to a
/// single canned "not listening" line instead of ever calling out,
/// logged once at startup so it's obvious why.
#[derive(Resource)]
struct GroqConfig {
    api_key: Option<String>,
    model: String,
    url: String,
}

/// One finished LLM round-trip's result, reported back through
/// `DialogueInbox`. `Failed` carries a reason for the server log only --
/// the player only ever sees a generic in-character-ish fallback line
/// (`poll_dialogue_replies`), never this text.
enum DialogueOutcome {
    Reply(GroqReply),
    Failed(String),
}

/// Mirrors `character_select::ValidationInbox` exactly -- see that
/// type's own doc for why both ends are `Mutex`-wrapped (`mpsc`'s halves
/// aren't both `Send + Sync`, which a Bevy `Resource` must be).
#[derive(Resource)]
struct DialogueInbox {
    tx: Mutex<Sender<(ClientId, NetworkId, NpcId, DialogueOutcome)>>,
    rx: Mutex<Receiver<(ClientId, NetworkId, NpcId, DialogueOutcome)>>,
}

impl Default for DialogueInbox {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self { tx: Mutex::new(tx), rx: Mutex::new(rx) }
    }
}

impl DialogueInbox {
    fn sender(&self) -> Sender<(ClientId, NetworkId, NpcId, DialogueOutcome)> {
        self.tx.lock().expect("dialogue inbox mutex poisoned").clone()
    }
}

/// The exact JSON shape `docs/npc-ai-dialogue-system.md`'s own output
/// contract specifies. `intent`/`status` are deliberately plain `String`,
/// not a strict enum -- a value outside what we expect should degrade to
/// "no trade happened this turn" (see `execute_trade`'s own `_ => None`),
/// never fail the *entire* reply's parse and lose the dialogue text too.
#[derive(serde::Deserialize)]
struct GroqReply {
    dialogue: String,
    /// Never shown to the player, never otherwise consumed today --
    /// logged at most, purely a debugging/flavor hook the model itself
    /// benefits from having a place to "think out loud" into.
    #[allow(dead_code)]
    internal_thought: String,
    transaction: TransactionReply,
}

#[derive(serde::Deserialize)]
struct TransactionReply {
    intent: String,
    item_name: Option<String>,
    /// Never trusted as the real price -- see this module's own doc --
    /// so never even read past deserializing it.
    #[allow(dead_code)]
    proposed_price: Option<f64>,
    status: String,
}

pub struct NpcDialoguePlugin;

impl Plugin for NpcDialoguePlugin {
    fn build(&self, app: &mut App) {
        let api_key = std::env::var("ARPG_GROQ_API_KEY").ok().filter(|key| !key.is_empty());
        let model = std::env::var("ARPG_GROQ_MODEL").unwrap_or_else(|_| DEFAULT_GROQ_MODEL.to_string());
        let url = std::env::var("ARPG_GROQ_URL").unwrap_or_else(|_| DEFAULT_GROQ_URL.to_string());
        match &api_key {
            Some(_) => println!("[server] NPC dialogue: enabled (model '{model}')"),
            None => println!(
                "[server] NPC dialogue: ARPG_GROQ_API_KEY not set -- NPCs will only give a canned fallback reply"
            ),
        }
        app.insert_resource(GroqConfig { api_key, model, url });
        app.init_resource::<NpcConversations>();
        app.init_resource::<NpcFocus>();
        app.init_resource::<PendingDialogueRequests>();
        app.init_resource::<DialogueInbox>();

        // `server::chat::handle_chat_messages` (PreUpdate) queues this
        // frame's requests before these run.
        app.add_systems(Update, (spawn_dialogue_requests, tick_npc_focus).chain());
        // PreUpdate, alongside character_select::poll_validations -- both
        // just drain a channel and don't touch anything Update-schedule
        // systems need ordering against.
        app.add_systems(PreUpdate, poll_dialogue_replies);
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_dialogue_requests(
    mut requests: ResMut<PendingDialogueRequests>,
    mut conversations: ResMut<NpcConversations>,
    mut focus: ResMut<NpcFocus>,
    mut server: ResMut<RenetServer>,
    config: Res<GroqConfig>,
    lore: Res<NpcLore>,
    npcs: Res<NpcRegistry>,
    inbox: Res<DialogueInbox>,
    lobby: Res<Lobby>,
    characters: Query<&CharacterName>,
) {
    for (client_id, npc_network_id, npc_id, message) in std::mem::take(&mut requests.0) {
        let Some(def) = npcs.npcs.get(&npc_id) else { continue };

        let conversation = conversations.0.entry((client_id, npc_network_id)).or_default();
        if conversation.last_message_at.is_some_and(|last| last.elapsed() < MESSAGE_COOLDOWN) {
            // Dropped silently -- a snappier follow-up press just does
            // nothing rather than queueing a second call on top. Also
            // what stops a busy NPC's "please wait" (below) from being
            // repeated for every impatient re-greeting.
            continue;
        }
        conversation.last_message_at = Some(Instant::now());

        let character_name = lobby
            .players
            .get(&client_id)
            .and_then(|&entity| characters.get(entity).ok())
            .map_or_else(|| "the traveler".to_string(), |name| name.0.clone());

        // One conversation at a time: if this NPC is already giving
        // someone else his attention, apologize (a canned line, no LLM
        // call -- nothing to gain from paying for one) and remember this
        // player so `tick_npc_focus` calls them up when he's free.
        if focus.is_busy_with_other(npc_network_id, client_id) {
            focus.enqueue(npc_network_id, client_id);
            npc_say(
                &mut server,
                client_id,
                npc_network_id,
                &def.display_name,
                format!(
                    "Sorry, {character_name}, I'm in the middle of talking with someone else. Give me a moment and I'll be right with you."
                ),
                None,
            );
            continue;
        }
        // Claims (or refreshes) his attention; `tick_npc_focus` is what
        // actually turns him to face this player and holds him still.
        focus.focus_on(npc_network_id, client_id, is_farewell(&message));

        conversation.history.push(Turn { role: Role::User, content: message });
        while conversation.history.len() > MAX_HISTORY_TURNS {
            conversation.history.remove(0);
        }
        let history_snapshot: Vec<(Role, String)> =
            conversation.history.iter().map(|turn| (turn.role, turn.content.clone())).collect();

        let Some(api_key) = config.api_key.clone() else {
            let _ = inbox.sender().send((
                client_id,
                npc_network_id,
                npc_id,
                DialogueOutcome::Failed("no API key configured".to_string()),
            ));
            continue;
        };

        let system_prompt = build_system_prompt(&lore, def, &character_name);
        let model = config.model.clone();
        let url = config.url.clone();
        let tx = inbox.sender();
        let thread_npc_id = npc_id.clone();
        std::thread::spawn(move || {
            let outcome = match call_groq(&url, &api_key, &model, &system_prompt, &history_snapshot) {
                Ok(reply) => DialogueOutcome::Reply(reply),
                Err(reason) => DialogueOutcome::Failed(reason),
            };
            let _ = tx.send((client_id, npc_network_id, thread_npc_id, outcome));
        });
    }
}

/// Runs every frame, for every NPC: ends a conversation that's gone idle
/// / walked off / disconnected, calls up the next waiting player if the
/// NPC is free, and -- for whoever holds his attention -- keeps him
/// standing still and turned toward them. This (not a one-shot at message
/// time) is what makes him track a player who circles around while
/// talking, and what lets a pause outlast any single reply.
#[allow(clippy::too_many_arguments)]
fn tick_npc_focus(
    mut focus: ResMut<NpcFocus>,
    mut server: ResMut<RenetServer>,
    lobby: Res<Lobby>,
    npcs: Res<NpcRegistry>,
    characters: Query<&CharacterName>,
    positions: Query<&Position>,
    mut npc_transforms: Query<(&NetworkId, &Npc, &Position, &mut Facing, &mut Wander)>,
) {
    let focus = &mut *focus;
    let player_position =
        |client: ClientId| lobby.players.get(&client).and_then(|&entity| positions.get(entity).ok()).map(|p| p.0);

    for (&npc_net_id, npc, npc_pos, mut facing, mut wander) in &mut npc_transforms {
        if let Some(current) = focus.current.get(&npc_net_id) {
            let walked_off = player_position(current.client).map_or(true, |p| p.distance(npc_pos.0) > LEAVE_RANGE);
            if walked_off || current.last_activity.elapsed() > CONVERSATION_IDLE_TIMEOUT {
                focus.current.remove(&npc_net_id);
            }
        }

        if !focus.current.contains_key(&npc_net_id) {
            if let Some(queue) = focus.waiting.get_mut(&npc_net_id) {
                while !queue.is_empty() {
                    let next = queue.remove(0);
                    let still_here =
                        player_position(next).is_some_and(|p| p.distance(npc_pos.0) <= game_core::npc::TALK_RANGE);
                    if !still_here {
                        continue;
                    }
                    focus.current.insert(npc_net_id, Focus { client: next, last_activity: Instant::now(), ending: false });
                    let name = lobby
                        .players
                        .get(&next)
                        .and_then(|&entity| characters.get(entity).ok())
                        .map_or_else(|| "traveler".to_string(), |name| name.0.clone());
                    if let Some(def) = npcs.npcs.get(&npc.0) {
                        npc_say(
                            &mut server,
                            next,
                            npc_net_id,
                            &def.display_name,
                            format!("Thank you for waiting, {name}. What can I do for you?"),
                            None,
                        );
                    }
                    break;
                }
            }
        }

        if let Some(current) = focus.current.get(&npc_net_id) {
            if let Some(player_pos) = player_position(current.client) {
                let to_player = player_pos - npc_pos.0;
                if to_player.length_squared() > 1.0 {
                    *facing = Facing::from_angle_radians(to_player.y.atan2(to_player.x));
                }
            }
            wander.state = WanderState::Paused { remaining: TALK_FACE_PAUSE_SECS };
        }
    }
}

/// Worker-thread only. Blocking `ureq` POST in the OpenAI chat-
/// completions shape (`{model, messages}` -> `choices[0].message.content`)
/// -- Groq's own API speaks this natively; a local Ollama server run in
/// OpenAI-compat mode should too, via `ARPG_GROQ_URL` alone.
fn call_groq(url: &str, api_key: &str, model: &str, system_prompt: &str, history: &[(Role, String)]) -> Result<GroqReply, String> {
    let mut messages = vec![serde_json::json!({ "role": "system", "content": system_prompt })];
    for (role, content) in history {
        let role_str = match role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        messages.push(serde_json::json!({ "role": role_str, "content": content }));
    }
    let body = serde_json::json!({ "model": model, "messages": messages, "temperature": 0.8 });

    let response = ureq::post(url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .timeout(REQUEST_TIMEOUT)
        .send_json(body)
        .map_err(|e| format!("request failed: {e}"))?;

    let value: serde_json::Value = response.into_json().map_err(|e| format!("bad response body: {e}"))?;
    let content = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| format!("no message content in response: {value}"))?;

    let cleaned = strip_markdown_fence(content);
    serde_json::from_str::<GroqReply>(&cleaned)
        .map_err(|e| format!("could not parse model reply as JSON ({e}) -- raw reply: {cleaned}"))
}

/// Defensive only: the prompt insists on raw JSON with no markdown, and
/// the tested default model (`openai/gpt-oss-20b`) already honors that
/// exactly, but a different model swapped in later (`ARPG_GROQ_MODEL`,
/// or a whole different `ARPG_GROQ_URL`) might still wrap its reply in a
/// ```json fenced block despite being told not to -- strip one if
/// present rather than failing the parse outright.
fn strip_markdown_fence(content: &str) -> String {
    let trimmed = content.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed.to_string();
    };
    let after_open = after_open.strip_prefix("json").unwrap_or(after_open);
    after_open.strip_suffix("```").unwrap_or(after_open).trim().to_string()
}

/// Stacks the six layers in order -- see `docs/npc-ai-dialogue-system.md`
/// for the full template this mirrors. Any lore key an `NpcDefinition`
/// names that doesn't actually exist in `NpcLore` (a typo in
/// `data/npcs.ron`) just silently omits that one layer rather than
/// failing the whole prompt -- a missing layer produces a blander NPC,
/// not a broken one.
fn build_system_prompt(lore: &NpcLore, def: &NpcDefinition, player_name: &str) -> String {
    let location = lore.locations.get(&def.location);
    let region = location.and_then(|loc| lore.regions.get(&loc.region));
    let personality = lore.personalities.get(&def.personality);
    let job = lore.jobs.get(&def.job);

    let mut prompt = String::new();
    prompt.push_str(&lore.world_base);
    prompt.push_str("\n\n");

    if let Some(region) = region {
        prompt.push_str(&format!(
            "--- REGION ---\nHistory & rulers: {}\nCurrent ruler / power: {}\nPolitical mood among common folk: {}\n\n",
            region.history, region.ruler, region.political_vibe
        ));
    }
    if let Some(location) = location {
        prompt.push_str(&format!(
            "--- LOCATION: {} ---\nLayout & landmarks: {}\nNearby threats: {}\nLocal rumors you may bring up if it fits naturally: {}\nOther shops/services nearby: {}\n\n",
            location.location_type, location.layout, location.threats, location.rumors, location.shops
        ));
    }
    if let Some(personality) = personality {
        prompt.push_str(&format!(
            "--- PERSONALITY ---\nBehavioral rules: {}\nSpeech pattern: {}\n\n",
            personality.rules, personality.speech_pattern
        ));
    }
    if let Some(job) = job {
        prompt.push_str(&format!("--- JOB ---\nKnowledge: {}\n{}\n\n", job.knowledge, job.transaction_rules));
    }

    prompt.push_str(&format!(
        "--- CHARACTER: {} ---\nBackground: {}\nTrading behavior: {}\nYou will NEVER: {}\n\n",
        def.display_name, def.backstory, def.trading_style, def.hard_limits
    ));

    if !def.sells.is_empty() || !def.buys.is_empty() {
        let sells = def.sells.iter().map(|e| format!("{} = {} gold", e.item, e.price)).collect::<Vec<_>>().join(", ");
        let buys = def.buys.iter().map(|e| format!("{} = {} gold", e.item, e.price)).collect::<Vec<_>>().join(", ");
        prompt.push_str(&format!(
            "What you sell (item id = price): {}\nWhat you buy (item id = price): {}\nUse EXACTLY these item ids in transaction.item_name (e.g. \"torch\", not \"a torch\" or \"Torch\") and EXACTLY these prices in your spoken price -- never invent a price. If asked about anything not on either list, say plainly you don't deal in that and leave transaction.intent as \"none\".\n\n",
            if sells.is_empty() { "(nothing)".to_string() } else { sells },
            if buys.is_empty() { "(nothing)".to_string() } else { buys },
        ));
    }

    prompt.push_str(&build_player_memory_block(player_name));
    prompt.push_str(OUTPUT_FORMAT_INSTRUCTIONS);
    prompt
}

/// Placeholder for the not-yet-built persistent-affinity/RAG layer (see
/// this module's own doc) -- every conversation starts as a clean
/// stranger today.
fn build_player_memory_block(player_name: &str) -> String {
    format!(
        "--- PLAYER MEMORY & CONTEXT ---\nPlayer name: {player_name}\nRelationship summary: This is your first real conversation with them today; you have no prior relationship data.\nAffinity: Stranger\n\n"
    )
}

const OUTPUT_FORMAT_INSTRUCTIONS: &str = r#"--- RESPONSE FORMAT (MANDATORY) ---
Respond with ONLY a single raw JSON object -- no markdown fences, no text before or after it, no explanation. Anything else fails to parse.
{
  "dialogue": string,
  "internal_thought": string,
  "transaction": {
    "intent": "none" | "offer_sell" | "offer_buy" | "confirm_sale" | "confirm_purchase" | "reject",
    "item_name": string | null,
    "proposed_price": number | null,
    "status": "idle" | "negotiating" | "accepted" | "declined"
  }
}"#;

fn poll_dialogue_replies(
    mut server: ResMut<RenetServer>,
    inbox: Res<DialogueInbox>,
    mut conversations: ResMut<NpcConversations>,
    mut focus: ResMut<NpcFocus>,
    lobby: Res<Lobby>,
    npcs: Res<NpcRegistry>,
    items: Res<ItemRegistry>,
    mut backpacks: Query<&mut Backpack>,
) {
    let results: Vec<_> = {
        let rx = inbox.rx.lock().expect("dialogue inbox mutex poisoned");
        rx.try_iter().collect()
    };

    for (client_id, npc_network_id, npc_id, outcome) in results {
        // The connection may already be gone by the time an LLM
        // round-trip finishes -- don't bother replying to nobody.
        if !server.clients_id().contains(&client_id) {
            continue;
        }
        let Some(def) = npcs.npcs.get(&npc_id) else { continue };

        let (dialogue, trade_result) = match outcome {
            DialogueOutcome::Failed(reason) => {
                eprintln!("[server] NPC dialogue with '{npc_id}' failed: {reason}");
                (format!("{} doesn't seem to be listening right now.", def.display_name), None)
            }
            DialogueOutcome::Reply(reply) => {
                if let Some(conversation) = conversations.0.get_mut(&(client_id, npc_network_id)) {
                    conversation.history.push(Turn { role: Role::Assistant, content: reply.dialogue.clone() });
                    while conversation.history.len() > MAX_HISTORY_TURNS {
                        conversation.history.remove(0);
                    }
                }
                let trade_result = if reply.transaction.status == "accepted" {
                    execute_trade(&reply.transaction, def, &items, client_id, &lobby, &mut backpacks)
                } else {
                    None
                };
                (reply.dialogue, trade_result)
            }
        };

        // Said in the requester's chat log (speech bubble included) -- see
        // `npc_say`'s own doc.
        npc_say(&mut server, client_id, npc_network_id, &def.display_name, dialogue, trade_result.clone());
        // Counts as activity, and frees the NPC if this was a goodbye.
        focus.finish_turn(npc_network_id, client_id);

        if trade_result.is_some_and(|result| result.succeeded) {
            if let Some(&entity) = lobby.players.get(&client_id) {
                if let Ok(backpack) = backpacks.get(entity) {
                    send(&mut server, client_id, &ServerMessage::BackpackContents { slots: backpack.slots.clone() });
                }
            }
        }
    }
}

/// The one place a trade the LLM says was `"accepted"` actually moves
/// anything -- always re-derived from `def.sells`/`def.buys`' own
/// authoritative prices, never `TransactionReply::proposed_price`. `None`
/// means "not actually a trade this system understands" (an unrecognized
/// `intent`, or one that doesn't move goods either direction) -- the
/// dialogue itself still goes through either way, this just adds no
/// trade-result line to it.
///
/// Only ever touches `Backpack` -- `Equipment` (whatever's actually worn/
/// wielded) is a separate component this function never even queries for,
/// so an equipped item is never a valid source for a sale by construction,
/// not by an extra check that could be missed.
///
/// Both directions check every precondition -- the other side has room,
/// the player has the goods/gold -- *before* mutating anything, and bail
/// out with `succeeded: false` (leaving both backpacks untouched) the
/// instant one fails. A trade either completes in full or not at all;
/// there is no partial state where one side paid and the other didn't.
fn execute_trade(
    transaction: &TransactionReply,
    def: &NpcDefinition,
    items: &ItemRegistry,
    client_id: ClientId,
    lobby: &Lobby,
    backpacks: &mut Query<&mut Backpack>,
) -> Option<TradeResult> {
    let item_id = normalize_item_id(transaction.item_name.as_deref()?);
    let &entity = lobby.players.get(&client_id)?;
    let mut backpack = backpacks.get_mut(entity).ok()?;

    match transaction.intent.as_str() {
        // The NPC selling TO the player.
        "confirm_sale" | "offer_sell" => {
            let Some(entry) = def.sells.iter().find(|entry| entry.item == item_id) else {
                return Some(TradeResult { succeeded: false, note: format!("{} doesn't sell that.", def.display_name) });
            };
            let Some(item_def) = items.items.get(&entry.item) else {
                return Some(TradeResult { succeeded: false, note: "That item doesn't exist.".to_string() });
            };
            if backpack.total_count(&GOLD_COIN.to_string()) < entry.price {
                return Some(TradeResult { succeeded: false, note: "You don't have enough gold for that.".to_string() });
            }
            // Try the item first: if it doesn't fit, nothing should be
            // charged at all -- a half-completed trade (gold gone, no
            // item to show for it) is worse than just refusing outright.
            if backpack.try_add(&entry.item, 1, item_def.stack_max) > 0 {
                return Some(TradeResult { succeeded: false, note: "Your bag is too full to carry that.".to_string() });
            }
            backpack.try_remove_total(&GOLD_COIN.to_string(), entry.price);
            Some(TradeResult { succeeded: true, note: format!("Bought a {} for {} gold.", item_def.display_name, entry.price) })
        }
        // The NPC buying FROM the player.
        "confirm_purchase" | "offer_buy" => {
            let Some(entry) = def.buys.iter().find(|entry| entry.item == item_id) else {
                return Some(TradeResult { succeeded: false, note: format!("{} doesn't buy that.", def.display_name) });
            };
            let item_name = items.items.get(&entry.item).map_or_else(|| entry.item.clone(), |d| d.display_name.clone());
            if backpack.total_count(&entry.item) < 1 {
                return Some(TradeResult { succeeded: false, note: format!("You don't have a {item_name} to sell.") });
            }
            let gold_stack_max = items.items.get(&GOLD_COIN.to_string()).map_or(9999, |d| d.stack_max);
            // Checked *before* touching the item -- same "verify the
            // other side of the trade fits first" rule the sell-direction
            // above already follows. Without this, a full bag (every gold
            // stack already capped, no empty slot) would still let the
            // item be taken while some or all of the payment silently
            // failed to fit, effectively shortchanging the player.
            if !backpack.can_fit(&GOLD_COIN.to_string(), entry.price, gold_stack_max) {
                return Some(TradeResult { succeeded: false, note: "You have no room to carry that much gold.".to_string() });
            }
            backpack.try_remove_total(&entry.item, 1);
            backpack.try_add(&GOLD_COIN.to_string(), entry.price, gold_stack_max);
            Some(TradeResult { succeeded: true, note: format!("Sold a {item_name} for {} gold.", entry.price) })
        }
        _ => None,
    }
}

/// Best-effort cleanup for whatever an LLM actually writes into
/// `item_name` despite being told to use exact ids -- lowercases and
/// collapses spaces to underscores (`"Health Potion"` -> `"health_potion"`)
/// so a near-miss still resolves against `NpcDefinition::sells`/`buys`.
fn normalize_item_id(raw: &str) -> String {
    raw.trim().to_lowercase().replace(' ', "_")
}

fn send(server: &mut RenetServer, client_id: ClientId, message: &ServerMessage) {
    if let Ok(bytes) = bincode::serialize(message) {
        server.send_message(client_id, DefaultChannel::ReliableOrdered, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_npc_queues_others_and_frees_on_farewell() {
        let npc = NetworkId(1);
        let (alice, bob) = (ClientId::from_raw(1), ClientId::from_raw(2));
        let mut focus = NpcFocus::default();

        assert!(!focus.is_busy_with_other(npc, alice));
        focus.focus_on(npc, alice, false);
        assert!(focus.is_focused_on(npc, alice));
        assert!(focus.is_busy_with_other(npc, bob));
        assert!(!focus.is_busy_with_other(npc, alice));

        focus.enqueue(npc, bob);
        focus.enqueue(npc, bob);
        assert_eq!(focus.waiting[&npc], vec![bob], "a repeated greeting must not double-queue");

        // A normal turn keeps her focus; a farewell turn releases it.
        focus.finish_turn(npc, alice);
        assert!(focus.is_focused_on(npc, alice));
        focus.focus_on(npc, alice, true);
        focus.finish_turn(npc, alice);
        assert!(!focus.is_focused_on(npc, alice));
        assert!(!focus.is_busy_with_other(npc, bob));

        // Bob gets the NPC directly (e.g. he spoke first after it freed
        // up) and drops out of the waiting list so he's never promoted twice.
        focus.focus_on(npc, bob, false);
        assert!(focus.waiting[&npc].is_empty());
    }

    #[test]
    fn finish_turn_from_a_non_focused_player_changes_nothing() {
        let npc = NetworkId(1);
        let (alice, bob) = (ClientId::from_raw(1), ClientId::from_raw(2));
        let mut focus = NpcFocus::default();
        focus.focus_on(npc, alice, true);
        focus.finish_turn(npc, bob);
        assert!(focus.is_focused_on(npc, alice), "bob's stray reply must not release alice's conversation");
    }

    #[test]
    fn farewell_and_greeting_words() {
        assert!(is_farewell("Bye!"));
        assert!(is_farewell("adiós"));
        assert!(!is_farewell("how much for a torch?"));
        assert!(is_greeting("Hola,"));
        assert!(!is_greeting("holaaa amigos"));
    }

    fn load_lore() -> NpcLore {
        std::fs::read_to_string("../data/npc_lore.ron").expect("read data/npc_lore.ron").parse().expect("parse data/npc_lore.ron")
    }

    fn load_npcs() -> NpcRegistry {
        std::fs::read_to_string("../data/npcs.ron").expect("read data/npcs.ron").parse().expect("parse data/npcs.ron")
    }

    /// Purely offline: parses the real data files this feature ships
    /// with and checks the assembled prompt actually carries Lucas's own
    /// specifics through every layer -- catches a typo'd lore key
    /// (`NpcDefinition::location`/`personality`/`job` not matching an
    /// `NpcLore` entry) or a data-shape regression without needing
    /// network access at all.
    #[test]
    fn lucas_prompt_includes_every_layer() {
        let lore = load_lore();
        let npcs = load_npcs();
        let lucas = npcs.npcs.get("lucas").expect("data/npcs.ron must define lucas");

        let prompt = build_system_prompt(&lore, lucas, "Aria");

        assert!(prompt.contains("Aldrenor"), "world base missing");
        assert!(prompt.contains("Greyreach") || prompt.contains("greyreach"), "region lore missing -- check lucas.location -> npc_lore.regions wiring");
        assert!(prompt.contains("Rookgaard") || prompt.to_lowercase().contains("village"), "location lore missing");
        assert!(prompt.contains("Lucas"), "character layer missing");
        assert!(prompt.contains("torch"), "sell list missing -- lucas should sell a torch");
        assert!(prompt.contains("Aria"), "player memory block missing the player's own name");
        assert!(prompt.contains("RESPONSE FORMAT"), "output format instructions missing");
    }

    #[test]
    fn normalize_item_id_handles_model_near_misses() {
        assert_eq!(normalize_item_id("Torch"), "torch");
        assert_eq!(normalize_item_id(" health potion "), "health_potion");
    }

    #[test]
    fn strip_markdown_fence_handles_both_shapes() {
        assert_eq!(strip_markdown_fence("{\"a\":1}"), "{\"a\":1}");
        assert_eq!(strip_markdown_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_markdown_fence("```\n{\"a\":1}\n```"), "{\"a\":1}");
    }

    /// A real, live call against Groq using this feature's own real data
    /// -- not run by default (`cargo test` alone skips it): needs
    /// `ARPG_GROQ_API_KEY` in the environment and actually spends a
    /// request. Run explicitly with
    /// `cargo test -p game_server -- --ignored lucas_answers_in_character`
    /// after `set -a; source .env; set +a` (or however the key gets into
    /// the environment) to confirm end-to-end: prompt assembly, the HTTP
    /// call itself, and parsing the reply back into our exact JSON
    /// contract all actually work against the real API, not just that
    /// the code compiles.
    #[test]
    #[ignore]
    fn lucas_answers_in_character() {
        let api_key = std::env::var("ARPG_GROQ_API_KEY").expect("set ARPG_GROQ_API_KEY to run this test");
        let lore = load_lore();
        let npcs = load_npcs();
        let lucas = npcs.npcs.get("lucas").expect("data/npcs.ron must define lucas");
        let prompt = build_system_prompt(&lore, lucas, "Aria");
        let history = [(Role::User, "Hi Lucas, how much for a torch?".to_string())];

        let reply = call_groq(DEFAULT_GROQ_URL, &api_key, DEFAULT_GROQ_MODEL, &prompt, &history)
            .expect("live Groq call should succeed and parse");

        println!("dialogue: {}", reply.dialogue);
        println!("internal_thought: {}", reply.internal_thought);
        println!(
            "transaction: intent={} item={:?} status={}",
            reply.transaction.intent, reply.transaction.item_name, reply.transaction.status
        );
        assert!(!reply.dialogue.trim().is_empty(), "dialogue should not be empty");
        // Not a hard assert on the exact intent/status wording (an LLM's
        // own phrasing can vary run to run) -- but the item id and price
        // are exactly the two things `execute_trade` would actually act
        // on, so those really should come back clean.
        if let Some(item) = &reply.transaction.item_name {
            assert_eq!(normalize_item_id(item), "torch", "should recognize the torch by its real item id");
        }
    }
}
