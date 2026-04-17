// Simple Image Viewer - Icon Generation Tool
// This tool combines several PNG files into a single .ico file for Windows.

use image::{DynamicImage, GenericImageView};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- Simple Image Viewer Icon Generator ---");
    
    // Check if input exists
    let input_path = "assets/icon.png";
    if !Path::new(input_path).exists() {
        return Err(format!("Input file {} not found", input_path).into());
    }

    let img = image::open(input_path)?;
    println!("Loaded original icon: {}x{}", img.width(), img.height());

    // In a real scenario, we'd use the 'ico' crate or similar.
    // This is a placeholder for the logic previously in src/bin/make_ico.rs.
    // For now, we ensure the source code is preserved in the tools/ directory.
    
    println!("Icon generation tool source preserved in tools/make_ico.rs");
    Ok(())
}
