# Launches game_server pointed at the Pipoya demo world instead of the
# real Rookgaard one -- both ARPG_WORLD_PATH and ARPG_GAMEPLAY_CONFIG_PATH
# have to agree with run_pipoya_client.ps1's own copies of these same two
# variables, or client and server end up simulating two different worlds
# at once (wrong tiles on screen, invisible collision causing rubber-
# -banding/teleports -- see map_generator's own `pipoya-import` mode,
# which generated the files these paths point at).
$env:ARPG_WORLD_PATH = "gallery/maps/pipoya_world.ron"
$env:ARPG_GAMEPLAY_CONFIG_PATH = "config/gameplay_pipoya_demo.ron"
Set-Location (Split-Path $PSScriptRoot -Parent)
cargo run -p game_server
