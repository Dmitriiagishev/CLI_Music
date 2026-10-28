use crate::backend::{Auth, Track};
use image::{ImageReader, Limits, RgbImage};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn cache_path(url: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let digest = Sha256::digest(url.as_bytes());
    let key: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Some(
        PathBuf::from(home)
            .join(".cli-music")
            .join("covers")
            .join(format!("{key}.png")),
    )
}
fn read_image(path: &Path) -> Result<RgbImage, String> {
    if fs::metadata(path).map_err(|e| e.to_string())?.len() > 10 * 1024 * 1024 {
        return Err("Обложка слишком большая".into());
    }
    let mut reader = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map(|image| image.thumbnail(256, 256).to_rgb8())
        .map_err(|_| "Не удалось прочитать обложку".into())
}
pub fn load(track: &Track, auth: &Auth) -> Result<RgbImage, String> {
    let cache = cache_path(&track.url);
    if let Some(path) = &cache {
        if let Ok(image) = read_image(path) {
            return Ok(image);
        }
    }
    let dir = tempfile::Builder::new()
        .prefix("cli-music-cover-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let template = dir.path().join("cover.%(ext)s");
    let mut cmd = Command::new("yt-dlp");
    if let Some(browser) = &auth.browser {
        cmd.args(["--cookies-from-browser", browser]);
    }
    if let Some(cookies) = &auth.cookies {
        cmd.args(["--cookies", cookies]);
    }
    let out = cmd
        .args([
            "--ignore-config",
            "--no-playlist",
            "--no-warnings",
            "--no-progress",
            "--skip-download",
            "--write-thumbnail",
            "-o",
        ])
        .arg(&template)
        .arg("--")
        .arg(&track.url)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("Не удалось загрузить обложку".into());
    }
    let path = fs::read_dir(dir.path())
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.extension().and_then(|x| x.to_str()).is_some_and(|ext| {
                ["jpg", "jpeg", "png", "webp"].contains(&ext.to_ascii_lowercase().as_str())
            })
        })
        .ok_or_else(|| "Обложка недоступна".to_owned())?;
    let image = read_image(&path)?;
    let size = image.width().min(image.height());
    let x = (image.width() - size) / 2;
    let y = (image.height() - size) / 2;
    let square = image::imageops::crop_imm(&image, x, y, size, size).to_image();
    // The cache is optional: failed writes never prevent playback or display.
    if let Some(path) = cache {
        if let Some(parent) = path.parent() {
            if fs::create_dir_all(parent).is_ok() {
                if let Ok(mut tmp) = tempfile::NamedTempFile::new_in(parent) {
                    if image::DynamicImage::ImageRgb8(square.clone())
                        .write_to(&mut tmp, image::ImageFormat::Png)
                        .is_ok()
                    {
                        let _ = tmp.persist(path);
                    }
                }
            }
        }
    }
    Ok(square)
}
