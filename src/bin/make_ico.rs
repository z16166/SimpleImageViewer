// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

/// One-shot ICO generator. Run with: cargo run --bin make_ico
fn main() {
    use std::path::Path;
    
    let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = project_dir.join("assets/icon.jpg");
    let dst = project_dir.join("assets/icon.ico");

    println!("Reading: {}", src.display());
    
    match png_to_ico(&src, &dst) {
        Ok(_) => println!("Created: {} ({} bytes)", dst.display(), std::fs::metadata(&dst).unwrap().len()),
        Err(e) => { eprintln!("Error: {e}"); std::process::exit(1); }
    }
}

fn png_to_ico(src: &std::path::Path, dst: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use image::imageops::FilterType;
    use image::ImageFormat;
    use std::fs::File;
    use std::io::{BufWriter, Write};

    let src_img = image::open(src)?;
    let sizes: &[u32] = &[16, 32, 48, 64, 128, 256];

    let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(sizes.len());
    for &sz in sizes {
        let scaled = src_img.resize_exact(sz, sz, FilterType::Lanczos3);
        let mut buf = Vec::new();
        scaled.write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Png)?;
        blobs.push(buf);
    }

    let count = sizes.len() as u16;
    let header_sz = 6u32;
    let entry_sz  = 16u32 * count as u32;
    let mut data_offset = header_sz + entry_sz;
    let mut offsets: Vec<u32> = Vec::new();
    for blob in &blobs {
        offsets.push(data_offset);
        data_offset += blob.len() as u32;
    }

    let mut out = BufWriter::new(File::create(dst)?);
    out.write_all(&0u16.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&count.to_le_bytes())?;
    for (i, &sz) in sizes.iter().enumerate() {
        let dim = if sz >= 256 { 0u8 } else { sz as u8 };
        out.write_all(&[dim, dim, 0u8, 0u8])?;
        out.write_all(&1u16.to_le_bytes())?;
        out.write_all(&32u16.to_le_bytes())?;
        out.write_all(&(blobs[i].len() as u32).to_le_bytes())?;
        out.write_all(&offsets[i].to_le_bytes())?;
    }
    for blob in &blobs { out.write_all(blob)?; }
    Ok(())
}
