use crate::backend::{Auth, Track};
use image::{ImageReader, Limits, RgbImage};
use std::{fs, process::Command};

// Keep downloads outside the repository, and leave playback independent
// of thumbnail availability. yt-dlp uses the same authentication as the track.
pub fn load(track: &Track, auth: &Auth) -> Result<RgbImage, String> {
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
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 10 * 1024 * 1024 {
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
        .map(|image| image.thumbnail(512, 512).to_rgb8())
        .map_err(|_| "Не удалось прочитать обложку".into())
}
