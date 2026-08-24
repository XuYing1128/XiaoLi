use image::RgbaImage;
use std::{env, fs, path::PathBuf};

const TRAY_SIZE: usize = 32;
const MASCOT_SIZE: usize = 27;
const ALPHA_THRESHOLD: u8 = 4;

fn main() {
    println!("cargo:rerun-if-changed=icons/tray-master.png");
    generate_tray_base().expect("failed to generate the embedded tray mascot");
    tauri_build::build()
}

fn generate_tray_base() -> Result<(), String> {
    let source = image::open("icons/tray-master.png")
        .map_err(|error| error.to_string())?
        .into_rgba8();
    let bounds =
        alpha_bounds(&source).ok_or_else(|| "tray master has no visible pixels".to_owned())?;
    let crop = square_crop(bounds, source.width() as usize, source.height() as usize);
    let mascot = resize_crop_premultiplied(&source, crop, MASCOT_SIZE);
    let mut canvas = vec![0_u8; TRAY_SIZE * TRAY_SIZE * 4];
    let offset_x = (TRAY_SIZE - MASCOT_SIZE) / 2;
    let offset_y = (TRAY_SIZE - MASCOT_SIZE) / 2;
    for y in 0..MASCOT_SIZE {
        let source_start = y * MASCOT_SIZE * 4;
        let target_start = ((y + offset_y) * TRAY_SIZE + offset_x) * 4;
        canvas[target_start..target_start + MASCOT_SIZE * 4]
            .copy_from_slice(&mascot[source_start..source_start + MASCOT_SIZE * 4]);
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is unavailable")?)
        .join("tray-base-32.rgba");
    fs::write(output, canvas).map_err(|error| error.to_string())
}

fn alpha_bounds(image: &RgbaImage) -> Option<(usize, usize, usize, usize)> {
    let (mut min_x, mut min_y) = (usize::MAX, usize::MAX);
    let (mut max_x, mut max_y) = (0_usize, 0_usize);
    let mut found = false;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] <= ALPHA_THRESHOLD {
            continue;
        }
        found = true;
        min_x = min_x.min(x as usize);
        min_y = min_y.min(y as usize);
        max_x = max_x.max(x as usize);
        max_y = max_y.max(y as usize);
    }
    found.then_some((min_x, min_y, max_x, max_y))
}

fn square_crop(
    bounds: (usize, usize, usize, usize),
    image_width: usize,
    image_height: usize,
) -> (usize, usize, usize) {
    let (min_x, min_y, max_x, max_y) = bounds;
    let content_width = max_x - min_x + 1;
    let content_height = max_y - min_y + 1;
    let content_size = content_width.max(content_height);
    let margin = ((content_size as f64) * 0.045).ceil() as usize;
    let side = (content_size + margin * 2).min(image_width.min(image_height));
    let center_x = (min_x + max_x) / 2;
    let center_y = (min_y + max_y) / 2;
    let x = center_x
        .saturating_sub(side / 2)
        .min(image_width.saturating_sub(side));
    let y = center_y
        .saturating_sub(side / 2)
        .min(image_height.saturating_sub(side));
    (x, y, side)
}

fn resize_crop_premultiplied(
    image: &RgbaImage,
    crop: (usize, usize, usize),
    output_size: usize,
) -> Vec<u8> {
    let (crop_x, crop_y, crop_size) = crop;
    let source = image.as_raw();
    let source_width = image.width() as usize;
    let mut output = vec![0_u8; output_size * output_size * 4];
    for output_y in 0..output_size {
        let source_y0 = crop_y + output_y * crop_size / output_size;
        let source_y1 = crop_y + ((output_y + 1) * crop_size).div_ceil(output_size);
        for output_x in 0..output_size {
            let source_x0 = crop_x + output_x * crop_size / output_size;
            let source_x1 = crop_x + ((output_x + 1) * crop_size).div_ceil(output_size);
            let mut alpha_sum = 0_u64;
            let mut premultiplied = [0_u64; 3];
            let mut samples = 0_u64;
            for source_y in source_y0..source_y1 {
                for source_x in source_x0..source_x1 {
                    let index = (source_y * source_width + source_x) * 4;
                    let alpha = source[index + 3] as u64;
                    alpha_sum += alpha;
                    premultiplied[0] += source[index] as u64 * alpha;
                    premultiplied[1] += source[index + 1] as u64 * alpha;
                    premultiplied[2] += source[index + 2] as u64 * alpha;
                    samples += 1;
                }
            }
            let target = (output_y * output_size + output_x) * 4;
            if let (Some(red), Some(green), Some(blue)) = (
                premultiplied[0].checked_div(alpha_sum),
                premultiplied[1].checked_div(alpha_sum),
                premultiplied[2].checked_div(alpha_sum),
            ) {
                output[target] = red as u8;
                output[target + 1] = green as u8;
                output[target + 2] = blue as u8;
            }
            output[target + 3] = (alpha_sum / samples) as u8;
        }
    }
    output
}
