from PIL import Image


matrix_size = (3, 3)
img = Image.open("objects/terrain/caves/animation.png")
width, height = img.size

# Calculate individual block dimensions
block_w = width // matrix_size[0]
block_h = height // matrix_size[1]

block_num = 0

# Loop through rows first (top to bottom), then columns (left to right)
for row in range(matrix_size[0]):
    for col in range(matrix_size[1]):
        left = col * block_w
        top = row * block_h
        right = left + block_w
        bottom = top + block_h
        
        # Crop out the individual block
        block = img.crop((left, top, right, bottom))
        
        # Save each block sequentially
        block.save(f"frame_{block_num}.png")
        block_num += 1
