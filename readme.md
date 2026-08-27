# arpg-skeleton

Esqueleto de proyecto para un Action-RPG 2D/2.5D estilo Wizard of Legend +
Dragon Nest, en Rust + Bevy, con separación estricta lógica/render y
servidor dedicado headless.

## ⚠️ Nota sobre este entorno (no aplica en tu máquina)

Este esqueleto fue escrito y revisado en un sandbox cuyo Rust del sistema
(`apt install cargo`) es la versión 1.75, ya vieja. El índice de crates.io
de **hoy** (mediados de 2026) ya tiene dependencias transitivas que
requieren `edition2024` (Rust 1.85+), así que `cargo check` en *este*
sandbox falla al descargar crates de terceros — no por errores en el
código de este repo. Con un Rust actual instalado vía `rustup` en tu PC
esto no debería pasar. Primer paso real:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
cd arpg-skeleton
cargo build
```

Si algo no compila con tu toolchain, probablemente sea porque Bevy 0.13
sacó una versión más nueva desde que armamos esto — revisá el changelog
de Bevy y ajustá versiones en los `Cargo.toml`.

## Estructura (por qué existe cada crate)

```
arpg-skeleton/
├── core/         <- game_core: la simulación. SOLO bevy_ecs/math/time/app.
│                    Cero render, cero window, cero audio. Esto es lo que
│                    corre IGUAL en cliente y servidor.
├── protocol/      <- Mensajes de red (serde). No sabe nada de Bevy salvo
│                    los tipos de datos que necesita serializar.
├── client/        <- Bevy completo (DefaultPlugins): ventana, sprites,
│                    input, audio. Depende de game_core pero SOLO lee su
│                    estado para dibujar -- nunca lo muta directamente.
└── server/        <- Bevy con default-features=false + MinimalPlugins:
                       mismo game_core, cero GPU/ventana/audio. Corre en
                       cualquier VPS de $5/mes.
```

**La regla de oro del proyecto**: si alguna vez tenés la tentación de meter
un `Sprite`, `Handle<Image>`, o cualquier cosa de `bevy_render` dentro de
`core/`, pará -- eso rompe el desacople que estás construyendo. `core/`
literalmente no puede compilar si intentás eso, porque `bevy_render` ni
siquiera está en su `Cargo.toml`. Usalo como red de seguridad.

## Cómo correrlo

```bash
# Terminal 1: servidor headless (autoridad de física/combate)
cargo run -p game_server

# Terminal 2: cliente con ventana
cargo run -p game_client
```

Ahora mismo no están conectados entre sí (falta la capa de networking,
ver TODOs abajo) -- cada uno corre su propia copia local de la simulación
para que puedas ver `game_core` funcionando de forma aislada.

## Qué ya está implementado

- **ECS con Bevy**: `Position`, `Velocity`, `Hurtbox`/`Hitbox`, `Health`,
  `Hitstop`, `Hitstun`, `IFrames`, `CombatState`.
- **El loop de combate central** (`core/src/systems/combat.rs`,
  `resolve_hitboxes`): detección de colisión AABB, daño, knockback
  (`launch` = tu sistema de juggles), y el freeze mutuo
  atacante/víctima en el impacto (`hitstop_frames`) al estilo Dragon
  Nest. Este es el sistema que más vas a iterar y tunear a mano.
- **Fixed timestep a 60hz** (`Time<Fixed>`), no atado al framerate de
  render -- crítico para que el combate se sienta igual en 30fps que en
  144fps, y para que cliente/servidor puedan comparar ticks.
- **Protocolo de red** (mensajes, no transporte todavía): `ClientInput`,
  `ServerMessage::Snapshot`, `HitConfirmed`.
- **InstanceId + TOWN_INSTANCE**: la base para separar el lobby social
  de las instancias de dungeon 2-4 jugadores, estilo Dragon Nest.

## Roadmap sugerido (orden de aprendizaje)

1. **Hacé andar el combate localmente primero.** Sin red. Agregá input
   de teclado en `client/`, un sistema que spawnee un `Hitbox` al
   apretar "atacar", y mirá `resolve_hitboxes` funcionar contra un dummy.
   Ajustá `hitstop_frames`/`hitstun_frames` hasta que "se sienta" bien --
   esto es 80% de por qué Wizard of Legend/Dragon Nest se sienten tan
   bien, y es puro tuning de números, no arquitectura.
2. **Agregá `renet`** (crate: `renet` + `renetcode`) al servidor y
   cliente para transporte UDP real. Empezá con un solo jugador
   controlado remotamente antes de pensar en predicción.
3. **Client-side prediction + reconciliation**: el cliente aplica su
   propio input localmente al toque (para que se sienta instantáneo) Y
   se lo manda al servidor. Cuando llega el snapshot del servidor,
   comparás tick contra tick y corregís si divergió. Esto es la parte
   más difícil del proyecto -- tomate tu tiempo, hay charlas de GDC
   sobre rollback netcode que valen mucho la pena antes de escribir
   código.
4. **Instancias**: usá `InstanceId` para filtrar qué snapshot le mandás
   a cada cliente (nunca mandes el estado de la instancia de otro grupo).
5. **Render de verdad**: reemplazá el cuadrado de placeholder por sprites
   pixel art reales, animaciones por `CombatState`, y ahí es donde entra
   tu estética Zelda/Ragnarok Online -- notá que llegás a este paso
   *último*, después de que el combate ya se siente bien, tal como
   pediste vos mismo (mecánicas antes que gráficos).

## Por qué Bevy ECS en vez de MVC clásico

Con combos/juggles vas a tener muchos "modificadores de estado
transitorios" por entidad (hitstun, iframes, combo counter, hitstop) que
en un `Modelo` tipo MVC clásico terminan siendo un montón de booleans/
timers dentro de una clase gigante `Player`. En ECS cada uno es un
componente independiente que un sistema simple (`tick_hitstop`,
`tick_iframes`) actualiza sin saber nada del resto -- se compone en vez
de heredar, y agregar un nuevo status effect (veneno, stun, lo que sea)
es agregar un componente + un sistema, no tocar una clase gorda.



NEXT steps:

Ready for Step 2 (chunking + the protocol/streaming/fog-of-war piece) whenever you want to move on — or let me know if you want to look at real tile art first, since everything's still flat-color placeholders.

Race/Profession registries + player components + leveling systems (no UI yet — verify via server logs/prints that XP→level-up→skill-unlock actually fires).
Backpack component (data only).
Sidebar UI rendering all of the above — biggest unknown since this project hasn't touched bevy_ui yet.

Chunked/streamed tile spawning — if generated maps keep growing, this is the real lever (spawn only tiles near players, like the server already does for creatures) rather than more visibility-toggling.
Remote-entity render smoothing — other players/creatures snap directly to snapshot positions with no interpolation, unlike your own predicted movement. Not reported as a problem, just an asymmetry worth knowing about.
I'd start with #1 and #2 since they're cheap to check and could silently corrupt your next map work. Want me to dig into either of those first?


Types of damage and defense:
phisical damage:
   - Blunt.
   - Pircing.

Magical:
   - Energy: raw magic, apply similar damage that blunt.
   - Fire.
   - Wind.
   - Water.
   - Earth.

Yes, that's a real and common technique — it's essentially 2D skeletal/cutout animation (what Spine, DragonBones, Unity's 2D Animation package, and Godot's Skeleton2D all do): separate part sprites, each with a pivot, driven by a bone hierarchy so you swap the torso for armor or the hand for a weapon without re-drawing every combination. Bevy doesn't have this built in, but it's very doable — you'd essentially build a small bone-transform hierarchy out of Transform/GlobalTransform parent-child relationships, or pull in an existing crate (there's bevy_spine/bevy_dragonbones if you want to author rigs in Spine/DragonBones directly rather than hand-rolling the runtime).

The tradeoff worth weighing against your current art pipeline specifically: your character sprites today are pre-rendered per-direction PNGs (characters/.../rotations/south.png etc.) — rotating a pixel-art limb sprite at arbitrary angles at runtime tends to look bad (blur/aliasing) unless the art is drawn for that, so a bone rig usually wants either vector-ish art or a much higher base resolution than what you have now. The more common fit for a pixel-art top-down ARPG with 8-direction sprite sheets is actually the "paper doll" layering approach instead — same idea (separate torso/head/weapon layers you can mix and match) but each part still has its own hand-drawn frame per direction/animation, composited by z-order instead of runtime rotation. That's more art-per-combination than a true rig, but it stays faithful to your existing rotation-folder pipeline rather than fighting it. Given you're already invested in per-direction PNGs, I'd lean toward investigating layered paper-doll compositing first and only reach for a true bone rig if the art style moves toward something rotation-friendly.

close port>
powershell -NoProfile -Command "Get-Process game_server -ErrorAction SilentlyContinue | Select-Object Id,ProcessName"

powershell -NoProfile -Command "Stop-Process -Id 6712 -Force"


