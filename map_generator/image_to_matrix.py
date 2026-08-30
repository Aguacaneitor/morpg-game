import numpy as np
from PIL import Image

# 1. Palette definition
COLOR_MAP = {
    1: ([204, 197, 184], "grass"),         # #ccc5b8
    2: ([96, 97, 56],    "dirt path"),     # #606138
    3: ([26, 25, 23],    "fortify walls"), # #1a1917
    4: ([165, 160, 149], "buildings"),     # #a5a095
    5: ([127, 122, 113], "water")          # #7f7a71
}

def process_map(image_path, block_size=10):
    img = Image.open(image_path).convert('RGB')
    img_np = np.array(img)
    
    img_h, img_w, _ = img_np.shape
    grid_h = img_h // block_size
    grid_w = img_w // block_size
    
    matrix = np.zeros((grid_h, grid_w), dtype=int)
    
    palette_values = list(COLOR_MAP.keys())
    palette_rgbs = np.array([COLOR_MAP[k][0] for k in palette_values])
    
    for row in range(grid_h):
        for col in range(grid_w):
            r_start, r_end = row * block_size, (row + 1) * block_size
            c_start, c_end = col * block_size, (col + 1) * block_size
            tile = img_np[r_start:r_end, c_start:c_end]
            
            pixels = tile.reshape(-1, 3)
            distances = np.linalg.norm(pixels[:, None, :] - palette_rgbs[None, :, :], axis=2)
            closest_color_indices = np.argmin(distances, axis=1)
            
            counts = np.bincount(closest_color_indices, minlength=len(palette_values))
            dominant_index = np.argmax(counts)
            
            matrix[row, col] = palette_values[dominant_index]
            
    return matrix

def save_matrix_to_txt(matrix, filename="town_matrix.txt"):
    """Saves the 2D matrix in list format [...]"""
    with open(filename, "w") as f:
        f.write("[\n")
        rows = [f"[{','.join(map(str, row))}]" for row in matrix]
        f.write(",\n".join(rows))
        f.write("\n]\n")


if __name__ == "__main__":
    town_matrix = process_map("C:/Users/Mela y Dani/Downloads/first_twon_2_editted.png", block_size=10)
    
    save_matrix_to_txt(town_matrix, "town_matrix.txt")
    print("Matrix successfully saved to town_matrix.txt!")