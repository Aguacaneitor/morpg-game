import numpy as np
from PIL import Image

# 1. Palette definition
COLOR_MAP = {
    "rookgaard": {
        1: ([12, 97, 0], "grass"),         # #0c6100
        2: ([85, 85, 84],    "dirt path"), # #555554
        3: ([75, 60, 0],    "wooden floor"),     # #4b3c00
        4: ([37, 37, 34],    "fortify walls"), # #252522
        6: ([121, 121, 8], "wooden fence"),     # #797908
        5: ([0, 212, 255], "water"),          # #00d4ff
        10: ([200, 162, 10], "cave_floor"),          # #00d4ff
    },
    "objects": {
        (8, 72, 238): "wooden_stairs",     # #0848ee
        (243, 2, 11): "fire_place"     # ·f3020b
    }
}

def process_map(image_path, zone_name, block_size=10):
    # 1. Convert to RGBA instead of RGB to preserve transparency data
    img = Image.open(image_path).convert('RGBA')
    img_np = np.array(img)
    
    img_h, img_w, _ = img_np.shape
    grid_h = img_h // block_size
    grid_w = img_w // block_size
    
    matrix = np.zeros((grid_h, grid_w), dtype=int)
    
    palette_values = list(COLOR_MAP[zone_name].keys())
    palette_rgbs = np.array([COLOR_MAP[zone_name][k][0] for k in palette_values])
    
    for row in range(grid_h):
        for col in range(grid_w):
            r_start, r_end = row * block_size, (row + 1) * block_size
            c_start, c_end = col * block_size, (col + 1) * block_size
            tile = img_np[r_start:r_end, c_start:c_end]
            
            # 2. Reshape to split color channels (RGB) from transparency channel (Alpha)
            pixels_rgba = tile.reshape(-1, 4)
            pixels_rgb = pixels_rgba[:, :3]   # First 3 values: R, G, B
            pixels_alpha = pixels_rgba[:, 3]  # 4th value: Alpha transparency
            
            # 3. Filter out completely transparent pixels (Alpha == 0)
            # This directly captures the "no color" background
            colored_pixel_mask = pixels_alpha > 0
            
            # If the block has absolutely no colored pixels, leave matrix as 0 and skip math
            if not np.any(colored_pixel_mask):
                matrix[row, col] = 0
                continue
                
            # Only do color matching calculations on pixels that actually have color!
            valid_pixels = pixels_rgb[colored_pixel_mask]
            
            distances = np.linalg.norm(valid_pixels[:, None, :] - palette_rgbs[None, :, :], axis=2)
            closest_color_indices = np.argmin(distances, axis=1)
            
            counts = np.bincount(closest_color_indices, minlength=len(palette_values))
            dominant_index = np.argmax(counts)
            
            matrix[row, col] = palette_values[dominant_index]
            
    return matrix


def write_ouput_file(message, filename, first=False):
    with open(filename, "w" if first else "a") as f:
            f.write(message)

def obj_map_process(image_file_name):
    # 1. Load your image and convert it to RGB mode
    # (This ensures every pixel has 3 values: Red, Green, Blue)
    img = Image.open(image_file_name).convert("RGB")

    # 2. Convert the image into a numpy matrix (Height x Width x Channels)
    # This perfectly maps rows to vertical positions and columns to horizontal positions.
    img_matrix = np.array(img)

    # 3. Define your color-to-object dictionary
    # Use RGB tuples as keys, and the target object name as values.
    color_to_object = COLOR_MAP.get("objects", [])

    # 4. Iterate over the matrix rows and columns
    rows, cols, channels = img_matrix.shape

    detected_objects = []

    for r in range(rows):
        for c in range(cols):
            # Get the RGB values of the current pixel matrix position
            pixel_color = tuple(img_matrix[r, c])
            
            # Check if this color is in your object dictionary
            if pixel_color in color_to_object:
                object_name = color_to_object[pixel_color]
                # Save the name along with its (row, column) matrix coordinates
                detected_objects.append({
                    "object": object_name,
                    "position": (r, c),
                    "color": pixel_color
                })

    # 5. Print out the results
    for item in detected_objects:
        print(f"Found {item['object']} at Row: {item['position'][0]}, Column: {item['position'][1]} (RGB: {item['color']})")

def save_matrix_to_txt(matrix, filename):
    """Saves the 2D matrix in list format [...]"""
    rows = [f"[{','.join(map(str, row))}]" for row in matrix]
    write_ouput_file(",\n".join(rows), filename)

if __name__ == "__main__":
    zone_name = "rookgaard"
    filename=f"{zone_name}_matrix.txt"
    for lvl_name in ["lvl-1_0"]:#["base", "lvl0_1", "lvl0_2", "lvl1_1", "lvl1_2"]:
        var1 = 0
        var2 = 0
        if lvl_name != "base":
            parts = lvl_name.replace("lvl", "").split("_")
            var1 = int(parts[0])
            var2 = int(parts[1])

        write_ouput_file(f"""(\n name: "{lvl_name}",
                    height: {var2},
                    floor: {var1},
                    grid: [\n""", filename, True if lvl_name == "base" else False)
        write_ouput_file(f"\n//lvl_name: {lvl_name}\n", filename)
        town_matrix = process_map(f"C:/Users/Mela y Dani/Desktop/game/morpg-game/map_generator/zones/{zone_name}/{lvl_name}.png", zone_name, block_size=1)
        save_matrix_to_txt(town_matrix, filename)
        write_ouput_file("\n]\n),\n", filename)
        
    for obj_lvl in ["lvl0_obj"]:
        obj_map_process(f"C:/Users/Mela y Dani/Desktop/game/morpg-game/map_generator/zones/{zone_name}/{obj_lvl}.png")
    write_ouput_file("\n]", filename)
    print("Matrix successfully saved to town_matrix.txt!")
