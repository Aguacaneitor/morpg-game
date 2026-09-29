# arpg-skeleton

Esqueleto de proyecto para un Action-RPG 2D/2.5D estilo Wizard of Legend +
Dragon Nest, en Rust + Bevy, con separación estricta lógica/render y
servidor dedicado headless.

## Requisitos

Rust estable, instalado con [rustup](https://rustup.rs). SQLite viene
compilado adentro y todo el contenido del juego está en el repo. En Linux
el cliente necesita además las librerías de sistema que pide la guía de
instalación de Bevy.

## Estructura (por qué existe cada crate)

```
arpg-skeleton/
├── core/          <- game_core: la simulación. SOLO bevy_ecs/math/time/app.
│                     Cero render, cero window, cero audio. Esto es lo que
│                     corre IGUAL en cliente y servidor.
├── protocol/      <- Mensajes de red y cómo se codifican. Usa tipos de
│                     game_core (ids, componentes), nada de render.
├── client/        <- Bevy completo (DefaultPlugins): ventana, sprites,
│                     input, audio. Depende de game_core pero SOLO lee su
│                     estado para dibujar -- nunca lo muta directamente.
├── server/        <- Bevy con default-features=false + MinimalPlugins:
│                     mismo game_core, cero GPU/ventana/audio. Corre en
│                     cualquier VPS de $5/mes.
├── auth_server/   <- Cuentas y sesiones (HTTP). Corre junto al servidor,
│                     nunca en la PC del jugador.
├── map_generator/ <- Herramienta para generar e importar zonas
│                     (gallery/maps).
└── xtask/         <- `cargo dev`: compila y corre todo junto para
                      desarrollar.
```

**La regla de oro del proyecto**: si alguna vez tenés la tentación de meter
un `Sprite`, `Handle<Image>`, o cualquier cosa de `bevy_render` dentro de
`core/`, pará -- eso rompe el desacople que estás construyendo. `core/`
literalmente no puede compilar si intentás eso, porque `bevy_render` ni
siquiera está en su `Cargo.toml`. Usalo como red de seguridad.

## Cómo correrlo

Todo junto (auth + servidor + ventana de juego) con una sola orden, desde
cualquier carpeta del repo:

```bash
cargo dev                  # compila los tres y los corre en esta consola
cargo dev --clients 2      # dos ventanas de juego (entrá con dos cuentas)
cargo dev --no-client      # solo auth + servidor (p. ej. un cliente en otra PC)
```

Cada línea sale con el nombre de su proceso (`auth |`, `server |`,
`client |`). Cerrar la ventana del juego apaga los servidores; Ctrl+C apaga
todo, y el servidor guarda a todos los personajes antes de salir.

Las herramientas de desarrollo del cliente (H: colisiones, L: más luz, F5:
subir un nivel, el botón de teletransporte) vienen en los builds normales;
el cliente que se le da a los jugadores se compila sin ellas:
`cargo build --release -p game_client --no-default-features`. El servidor
solo acepta la subida de nivel y el teletransporte con
`ARPG_DEBUG_COMMANDS=1` (`cargo dev` lo activa; Docker no). `cargo dev` es solo para desarrollo: en producción el servidor y el
auth corren en un host y cada jugador abre su propio cliente.

Por separado sigue funcionando igual (`cargo run -p auth_server`,
`cargo run -p game_server`, `cargo run -p game_client`). Los binarios
encuentran `config/`, `data/` y `gallery/` solos, aunque los abras directo
desde `target/debug/`; la variable `ARPG_ROOT` fuerza otra carpeta.

### Medir rendimiento

- **Cliente:** F3 muestra FPS y el tiempo de frame promedio y peor del
  último segundo; mientras está visible, cada tirón de más de 50 ms se
  escribe en la consola con la posición del jugador.
- **Servidor:** escribe una línea (como mucho cada 10 s) cuando algún frame
  tarda más que un paso de simulación (16,7 ms).
- **Por sistema:** `cargo run -p game_client --features bevy/trace_tracy`
  (o `-p game_server`) y conectá el profiler [Tracy](https://github.com/wolfpld/tracy).
  La primera vez descarga las dependencias de Tracy.

### Hostearlo (Docker)

`docker compose up --build -d` levanta `auth_server` (puerto 5001/tcp) y
`game_server` (5000/udp). Cuentas y personajes quedan en volúmenes, así que
sobreviven a reinicios y rebuilds, y `docker compose stop`/`down` guarda a
los jugadores conectados antes de apagar el servidor. La clave de Groq para los NPCs se toma
de tu `.env` o de la consola y nunca entra a la imagen. Cada jugador abre
su cliente apuntando al host:

```bash
ARPG_AUTH_URL=http://<host>:5001 ARPG_SERVER_ADDR=<host>:5000 game_client
```

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
- **Red** (`renet`, UDP): el cliente predice su propio movimiento y lo
  reconcilia con el servidor; a los demás los dibuja interpolando entre
  snapshots (30 por segundo). El servidor solo le manda a cada jugador lo
  que puede ver: radio de visión, paredes, pisos y luces.
- **Cuentas y personajes**: `auth_server` (registro, login, sesiones) y
  pantalla de selección/creación de personaje; todo se guarda en SQLite
  (`saves/`).
- **Mundo**: zonas con varios pisos (puentes, escaleras), día y noche,
  visión y luces, NPCs con diálogo por LLM, chat, loot, inventario,
  equipo, habilidades y profesiones.
- **InstanceId + TOWN_INSTANCE**: la base para separar el lobby social
  de las instancias de dungeon 2-4 jugadores, estilo Dragon Nest.

## Roadmap sugerido (orden de aprendizaje)

1. ✅ **Hacé andar el combate localmente primero.** Sin red. Agregá input
   de teclado en `client/`, un sistema que spawnee un `Hitbox` al
   apretar "atacar", y mirá `resolve_hitboxes` funcionar contra un dummy.
   Ajustá `hitstop_frames`/`hitstun_frames` hasta que "se sienta" bien --
   esto es 80% de por qué Wizard of Legend/Dragon Nest se sienten tan
   bien, y es puro tuning de números, no arquitectura.
2. ✅ **Agregá `renet`** (crate: `renet` + `renetcode`) al servidor y
   cliente para transporte UDP real. Empezá con un solo jugador
   controlado remotamente antes de pensar en predicción.
3. ✅ **Client-side prediction + reconciliation**: el cliente aplica su
   propio input localmente al toque (para que se sienta instantáneo) Y
   se lo manda al servidor. Cuando llega el snapshot del servidor,
   comparás tick contra tick y corregís si divergió. Esto es la parte
   más difícil del proyecto -- tomate tu tiempo, hay charlas de GDC
   sobre rollback netcode que valen mucho la pena antes de escribir
   código.
4. **Instancias**: usá `InstanceId` para filtrar qué snapshot le mandás
   a cada cliente (nunca mandes el estado de la instancia de otro grupo).
   El filtrado ya está; falta crear instancias de dungeon.
5. ✅ **Render de verdad**: reemplazá el cuadrado de placeholder por sprites
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


