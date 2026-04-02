use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("assets/icon.jpg").display()
    );

    // Generate the ICO from the source image (JPEG)
    let src = manifest_dir.join("assets/icon.jpg");
    let dst = manifest_dir.join("assets/icon.ico");

    if src.exists() {
        match png_to_ico(&src, &dst) {
            Ok(_) => println!("cargo:warning=icon.ico generated from icon.png"),
            Err(e) => eprintln!("build.rs: icon conversion failed: {e}"),
        }
    } else {
        eprintln!("build.rs: assets/icon.png not found, skipping ICO generation");
    }

    // Embed Windows resources (icon + metadata) into the PE
    #[cfg(target_os = "windows")]
    embed_resources(&dst);
}

/// Convert a PNG to a multi-resolution ICO (16, 32, 48, 64, 128, 256 px).
fn png_to_ico(src: &std::path::Path, dst: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use image::imageops::FilterType;
    use image::ImageFormat;
    use std::fs::File;
    use std::io::{BufWriter, Write};

    let src_img = image::open(src)?;
    let sizes: &[u32] = &[16, 32, 48, 64, 128, 256];

    // Encode each size as a PNG blob (PNG-in-ICO, supported Windows Vista+)
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

    let mut offsets: Vec<u32> = Vec::with_capacity(blobs.len());
    for blob in &blobs {
        offsets.push(data_offset);
        data_offset += blob.len() as u32;
    }

    let mut out = BufWriter::new(File::create(dst)?);

    // ICONDIR
    out.write_all(&0u16.to_le_bytes())?;  // reserved
    out.write_all(&1u16.to_le_bytes())?;  // type = ICON
    out.write_all(&count.to_le_bytes())?;

    // ICONDIRENTRY array
    for (i, &sz) in sizes.iter().enumerate() {
        let dim = if sz >= 256 { 0u8 } else { sz as u8 };
        out.write_all(&[dim, dim, 0u8, 0u8])?;
        out.write_all(&1u16.to_le_bytes())?;
        out.write_all(&32u16.to_le_bytes())?;
        out.write_all(&(blobs[i].len() as u32).to_le_bytes())?;
        out.write_all(&offsets[i].to_le_bytes())?;
    }

    for blob in &blobs {
        out.write_all(blob)?;
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn embed_resources(ico_path: &std::path::Path) {
    let mut res = winresource::WindowsResource::new();

    if ico_path.exists() {
        res.set_icon(&ico_path.display().to_string());
    }

    res.set("ProductName",     "Simple Image Viewer");
    res.set("FileDescription", "Simple Image Viewer");
    res.set("LegalCopyright",  "\u{a9} 2026");
    res.set("Comments",        "https://github.com/z16166/SimpleImageViewer/");

    if let Err(e) = res.compile() {
        eprintln!("build.rs: winresource error: {e}");
    }
}
