import random

def generate_autotile_maze(rows, cols, floor_tile=13, empty_tile=0):
    # Initialize grid entirely with empty spaces
    maze = [[empty_tile for _ in range(cols)] for _ in range(rows)]
    visited = set()
    
    # 1. Carve paths using the randomized Prim/DFS hybrid approach
    start_r, start_c = 1, 1
    maze[start_r][start_c] = 1  # Temporary marker for path
    visited.add((start_r, start_c))
    walls = []
    
    def add_walls(r, c):
        directions = [(-2, 0, -1, 0), (2, 0, 1, 0), (0, -2, 0, -1), (0, 2, 0, 1)]
        for dr, dc, mr, mc in directions:
            nr, nc = r + dr, c + dc
            if 0 < nr < rows - 1 and 0 < nc < cols - 1:
                if (nr, nc) not in visited:
                    walls.append((nr, nc, r + mr, c + mc))

    add_walls(start_r, start_c)
    
    while walls:
        # FIX: Select and remove using the actual 4-tuple element structure
        chosen_wall = random.choice(walls)
        walls.remove(chosen_wall)
        
        r, c, mr, mc = chosen_wall
        
        if (r, c) not in visited:
            visited.add((r, c))
            thick = random.choice([1, 2]) # 1 or 2 size path thickness
            
            maze[r][c] = 1
            maze[mr][mc] = 1
            
            if thick == 2:
                if mr == r: # Horizontal path: widen downwards
                    if r + 1 < rows - 1:
                        maze[r + 1][c] = 1
                        maze[mr + 1][mc] = 1
                else: # Vertical path: widen rightwards
                    if c + 1 < cols - 1:
                        maze[r][c + 1] = 1
                        maze[mr][mc + 1] = 1
            add_walls(r, c)

    # 2. Run Auto-Tiling Bitmask Engine to calculate exact index (1-16)
    final_maze = [[empty_tile for _ in range(cols)] for _ in range(rows)]
    
    for r in range(rows):
        for c in range(cols):
            if maze[r][c] == 1:
                final_maze[r][c] = floor_tile # Standard floor tile index (e.g., 13)
                continue
                
            # Evaluate surrounding context for border tiles
            up    = 1 if (r > 0 and maze[r-1][c] == 1) else 0
            down  = 1 if (r < rows - 1 and maze[r+1][c] == 1) else 0
            left  = 1 if (c > 0 and maze[r][c-1] == 1) else 0
            right = 1 if (c < cols - 1 and maze[r][c+1] == 1) else 0
            
            # Binary flag generation based on layout adjacency
            mask = (up * 8) + (down * 4) + (left * 2) + right
            
            # Map calculation into your 1-16 tileset space
            if mask == 0:
                final_maze[r][c] = empty_tile 
            else:
                tile_mapping = {
                    12: 1,   # Path is UP and DOWN (Left vertical border)
                    9:  9,   # Path is UP and RIGHT (Bottom-Left corner wall)
                    10: 16,   # Path is UP and LEFT (Bottom-Right corner wall)
                    14: 4,   # Path is UP, DOWN, LEFT
                    5:  5,   # Path is DOWN and RIGHT (Top-Left upper wall)
                    6:  6,   # Path is DOWN and LEFT (Top-Right upper wall)
                    7:  7,   # Path is DOWN, LEFT, RIGHT
                    15: 8,   # Path surrounds tile completely
                    3:  9,   # Path is LEFT and RIGHT (Top horizontal border)
                    11: 10,  # Path is UP, LEFT, RIGHT
                    13: 11,  # Path is UP, DOWN, RIGHT
                    4:  4,  # Path is DOWN only (Upper edge capping)
                    8:  10,  # Path is UP only (Lower flat floor shadow)
                    1:  2,  # Path is RIGHT only
                    2:  12   # Path is LEFT only
                }
                final_maze[r][c] = tile_mapping.get(mask, empty_tile)
                
    return final_maze

# Run and verify grid generation safely
GRID_ROWS, GRID_COLS = 22, 30
result_matrix = generate_autotile_maze(GRID_ROWS, GRID_COLS)

for row in result_matrix:
    print("[", end="")
    print(", ".join(f"{val:2d}" for val in row), end="")
    print("],")
