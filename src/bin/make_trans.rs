use image::{GenericImageView, ImageBuffer, Rgba};
fn main() {
    let src = "C:\\Users\\zhang\\.gemini\\antigravity\\brain\\997ed693-3a65-48bd-8fac-9810c0690265\\luxury_glass_icon_1775309872010.png";
    let dst = "f:\\Rust\\SimpleImageViewer\\assets\\icon.png";
    let img = image::io::Reader::open(src).unwrap().with_guessed_format().unwrap().decode().unwrap();
    let (w, h) = img.dimensions();
    let mut out = ImageBuffer::new(w, h);
    
    // Simple 4-way flood fill from all edge points to discover background pixels
    let mut bg = vec![false; (w * h) as usize];
    let mut stack = Vec::new();
    for x in 0..w {
        stack.push((x, 0));
        stack.push((x, h-1));
    }
    for y in 0..h {
        stack.push((0, y));
        stack.push((w-1, y));
    }
    
    while let Some((x, y)) = stack.pop() {
        let idx = (y * w + x) as usize;
        if bg[idx] { continue; }
        
        let px = img.get_pixel(x, y);
        // If near white
        let diff = (255 - px[0] as i32).abs() + (255 - px[1] as i32).abs() + (255 - px[2] as i32).abs();
        if diff < 180 { // Tolerance for near-white artifacting in AI images
            bg[idx] = true;
            if x > 0 { stack.push((x - 1, y)); }
            if x < w - 1 { stack.push((x + 1, y)); }
            if y > 0 { stack.push((x, y - 1)); }
            if y < h - 1 { stack.push((x, y + 1)); }
        }
    }
    
    // Smooth the edges and apply alpha
    for y in 0..h {
        for x in 0..w {
            let px = img.get_pixel(x, y);
            let idx = (y * w + x) as usize;
            if bg[idx] {
                // Completely transparent background
                out.put_pixel(x, y, Rgba([255, 255, 255, 0]));
            } else {
                // Foreground edge anti-aliasing loosely: if neighbors are background, blend alpha.
                let mut bg_count = 0;
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx >= 0 && nx < w as i32 && ny >= 0 && ny < h as i32 {
                            if bg[(ny * w as i32 + nx) as usize] {
                                bg_count += 1;
                            }
                        } else {
                             bg_count += 1; // Out of bounds is background
                        }
                    }
                }
                
                let mut alpha = 255;
                if bg_count > 0 {
                    let factor = (9 - bg_count) as f32 / 9.0;
                    // square curve for sharper falloff
                    alpha = (255.0 * factor * factor) as u8;
                }
                out.put_pixel(x, y, Rgba([px[0], px[1], px[2], alpha]));
            }
        }
    }
    
    out.save(dst).unwrap();
    // Copy to jpg just in case build.rs looks for it
    let dst_jpg = "f:\\Rust\\SimpleImageViewer\\assets\\icon.jpg";
    std::fs::copy(dst, dst_jpg).unwrap_or(0);
    println!("Transparent image saved to {} and {}", dst, dst_jpg);
}
