from pathlib import Path
import shutil

# --- CONFIGURATION ---
SOURCE_DIR = r"C:/Users/Mela y Dani/Downloads/magic/objects"
OUTPUT_DIR = r"C:/Users/Mela y Dani/Desktop/game/morpg-game/gallery/magic/sprites"
# ---------------------

def process_images(src_folder, out_folder):
    src_path = Path(src_folder)
    out_path = Path(out_folder)
    
    # Create the output directory if it doesn't exist
    out_path.mkdir(parents=True, exist_ok=True)
    
    # Iterate through each folder in the top directory (e.g., Flame_icon, fire_ball)
    for first_subfolder in src_path.iterdir():
        if first_subfolder.is_dir():
            # Search for any PNG file inside subfolders of the main item directory
            # Handles pattern: <first_subfolder>/<static_subfolder>/*.png
            png_files = list(first_subfolder.glob("*/*.png"))
            
            for img_path in png_files:
                # Use the first subfolder name + original extension for the new filename
                new_filename = f"{first_subfolder.name}{img_path.suffix}"
                destination = out_path / new_filename
                
                # Copy file to output directory
                shutil.copy2(img_path, destination)
                print(f"Copied: {img_path.name} -> {destination}")

if __name__ == "__main__":
    process_images(SOURCE_DIR, OUTPUT_DIR)