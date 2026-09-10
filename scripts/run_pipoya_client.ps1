# Launches game_client pointed at the Pipoya demo world -- see
# run_pipoya_server.ps1's own comment: this must always be started with
# the *server* also running via that script (or the same two env vars set
# by hand on both), never against a plain `cargo run -p game_server` --
# a client/server world mismatch shows up as wrong tiles on screen plus
# invisible collision (the server pushing against terrain the client
# never rendered), not a clean error either side would catch on its own.
$env:ARPG_WORLD_PATH = "gallery/maps/pipoya_world.ron"
$env:ARPG_GAMEPLAY_CONFIG_PATH = "config/gameplay_pipoya_demo.ron"
Set-Location (Split-Path $PSScriptRoot -Parent)
cargo run -p game_client
