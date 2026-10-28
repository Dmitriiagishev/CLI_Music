use crate::backend::{SearchEntry, Track};
use std::{env, process::Command};

const ADAPTER: &str = include_str!("../assets/ytmusic.py");
fn run(mode: &str, value: &str, limit: usize) -> Result<serde_json::Value, String> {
    let python = env::var_os("CLI_MUSIC_PYTHON").unwrap_or_else(|| {
        #[cfg(windows)]
        let path = std::path::Path::new(".venv/Scripts/python.exe");
        #[cfg(not(windows))]
        let path = std::path::Path::new(".venv/bin/python");
        if path.is_file() {
            path.as_os_str().to_owned()
        } else {
            #[cfg(windows)]
            {
                "python".into()
            }
            #[cfg(not(windows))]
            {
                "python3".into()
            }
        }
    });
    let output = Command::new(python)
        .args(["-c", ADAPTER, mode, value, &limit.to_string()])
        .output()
        .map_err(|e| format!("Не удалось запустить Python: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("Неверный ответ YouTube Music: {e}"))
}
pub fn search(query: &str, limit: usize) -> Result<Vec<SearchEntry>, String> {
    let (mode, value) = query
        .strip_prefix("musicartist:")
        .map(|id| ("artist", id))
        .unwrap_or(("search", query));
    let data = run(mode, value, limit)?;
    let entries = data.as_array().ok_or("Неверный формат результатов")?;
    let mut results = Vec::new();
    for entry in entries {
        let Some(title) = entry["title"].as_str() else {
            continue;
        };
        let Some(url) = entry["url"].as_str() else {
            continue;
        };
        match entry["type"].as_str() {
            Some("artist") if url.starts_with("musicartist:") => {
                results.push(SearchEntry::Artist {
                    title: title.to_owned(),
                    url: url.to_owned(),
                })
            }
            Some("track") if url.starts_with("https://music.youtube.com/watch?v=") => {
                results.push(SearchEntry::Track(Track {
                    title: title.to_owned(),
                    url: url.to_owned(),
                }))
            }
            _ => {}
        }
    }
    Ok(results)
}
pub fn lyrics(url: &str) -> Result<String, String> {
    let id = url
        .split("v=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .filter(|s| !s.is_empty())
        .ok_or("Текст для этой ссылки недоступен")?;
    run("lyrics", id, 1)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "Неверный формат текста песни".into())
}
