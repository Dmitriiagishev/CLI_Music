use std::collections::VecDeque;
use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub url: String,
}

#[derive(Clone, Default)]
pub struct Auth {
    pub browser: Option<String>,
    pub cookies: Option<String>,
}
impl Auth {
    fn apply(&self, cmd: &mut Command) {
        if let Some(browser) = &self.browser {
            cmd.args(["--cookies-from-browser", browser]);
        }
        if let Some(path) = &self.cookies {
            cmd.args(["--cookies", path]);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEntry {
    Artist { title: String, url: String },
    Track(Track),
}
impl SearchEntry {
    pub fn title(&self) -> &str {
        match self {
            Self::Artist { title, .. } => title,
            Self::Track(track) => &track.title,
        }
    }
}
fn parse_results(output: &str) -> Vec<SearchEntry> {
    let mut artists = Vec::new();
    let mut tracks = Vec::new();
    for line in output.lines() {
        let fields: Vec<_> = line.splitn(4, '\t').collect();
        if fields.len() != 4 {
            continue;
        }
        let (kind, id, title, entry_url) = (fields[0], fields[1], fields[2], fields[3]);
        if id.is_empty() || title.is_empty() || id == "NA" || title == "NA" {
            continue;
        }
        let is_artist = kind == "YoutubeTab"
            && (entry_url.contains("/channel/")
                || entry_url.contains("/@")
                || entry_url.contains("/user/")
                || id.starts_with("UC"));
        if is_artist {
            let base = if entry_url.starts_with("https://") {
                entry_url
                    .split('?')
                    .next()
                    .unwrap_or(entry_url)
                    .trim_end_matches('/')
                    .to_owned()
            } else {
                format!("https://www.youtube.com/channel/{id}")
            };
            let url = if base.ends_with("/videos") {
                base
            } else {
                format!("{base}/videos")
            };
            artists.push(SearchEntry::Artist {
                title: title.to_owned(),
                url,
            });
        } else if kind != "YoutubeTab" {
            let url = if entry_url.starts_with("https://") {
                entry_url.to_owned()
            } else {
                format!("https://www.youtube.com/watch?v={id}")
            };
            tracks.push(SearchEntry::Track(Track {
                title: title.to_owned(),
                url,
            }));
        }
    }
    artists.extend(tracks);
    artists
}
pub fn lookup(
    query: &str,
    auth: &Auth,
    limit: usize,
) -> Result<(Vec<SearchEntry>, String), String> {
    if !query.starts_with("https://") && !query.starts_with("http://") {
        return crate::music::search(query, limit).map(|results| (results, String::new()));
    }
    let mut cmd = Command::new("yt-dlp");
    auth.apply(&mut cmd);
    let limit = limit.clamp(1, 100);
    let target = if query.starts_with("https://") || query.starts_with("http://") {
        query.to_owned()
    } else {
        format!("ytsearch{limit}:{query}")
    };
    let output = cmd
        .args([
            "--ignore-config",
            "--flat-playlist",
            "--playlist-end",
            &limit.to_string(),
            "--print",
            "%(ie_key)s\t%(id)s\t%(title)s\t%(url)s",
            "--",
            &target,
        ])
        .output()
        .map_err(|e| format!("Не удалось запустить yt-dlp: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Ошибка yt-dlp: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let warnings = String::from_utf8_lossy(&output.stderr);
    Ok((
        parse_results(&String::from_utf8_lossy(&output.stdout)),
        warnings.trim().to_owned(),
    ))
}

fn resolve(track: &Track, auth: &Auth) -> Result<String, String> {
    let mut cmd = Command::new("yt-dlp");
    auth.apply(&mut cmd);
    let out = cmd
        .args([
            "--ignore-config",
            "--no-playlist",
            "--no-warnings",
            "-f",
            "bestaudio/best",
            "-g",
            "--",
            &track.url,
        ])
        .output()
        .map_err(|e| format!("Не удалось запустить yt-dlp: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    let url = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if url.is_empty() {
        Err("yt-dlp не вернул аудиопоток".into())
    } else {
        Ok(url)
    }
}

static NEXT_PLAYER: AtomicU64 = AtomicU64::new(0);

struct Player {
    child: Child,
    endpoint: String,
    #[cfg(unix)]
    directory: std::path::PathBuf,
    spectrum: Option<crate::spectrum::Spectrum>,
    stream_url: String,
}

impl Player {
    fn start(
        url: &str,
        volume: i32,
        events: Sender<Event>,
        enable_spectrum: bool,
    ) -> io::Result<Self> {
        let id = NEXT_PLAYER.fetch_add(1, Ordering::Relaxed);
        let name = format!("cli-music-{}-{id}", std::process::id());
        #[cfg(unix)]
        let (endpoint, directory) = {
            use std::os::unix::fs::DirBuilderExt;
            let dir = env::temp_dir().join(&name);
            std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
            (dir.join("mpv.sock").to_string_lossy().into_owned(), dir)
        };
        #[cfg(windows)]
        let endpoint = format!(r"\\.\pipe\{name}");
        let spectrum = if enable_spectrum {
            crate::spectrum::Spectrum::new()
                .ok()
                .filter(|s| s.graph().is_some())
        } else {
            None
        };
        if enable_spectrum && spectrum.is_none() {
            let _ = events.send(Event::Message(
                "Анализатор частот недоступен на этой системе".into(),
            ));
        }
        let mut command = Command::new("mpv");
        if let Some(graph) = spectrum.as_ref().and_then(|s| s.graph()) {
            command.arg(format!("--af=lavfi=[{graph}]"));
        }
        let child = command
            .args([
                "--no-video",
                "--input-terminal=no",
                "--msg-level=all=warn",
                &format!("--input-ipc-server={endpoint}"),
                &format!("--volume={volume}"),
                "--",
                url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match child {
            Ok(mut child) => {
                if let Some(stream) = child.stdout.take() {
                    forward_output(stream, events.clone());
                }
                if let Some(stream) = child.stderr.take() {
                    forward_output(stream, events.clone());
                }
                Ok(Self {
                    child,
                    endpoint,
                    #[cfg(unix)]
                    directory,
                    spectrum,
                    stream_url: url.to_owned(),
                })
            }
            Err(e) => {
                #[cfg(unix)]
                let _ = std::fs::remove_dir(&directory);
                Err(e)
            }
        }
    }

    fn command(&self, command: &str) -> Result<(), String> {
        // mpv creates the endpoint asynchronously after startup.
        for _ in 0..20 {
            #[cfg(unix)]
            let connection = std::os::unix::net::UnixStream::connect(&self.endpoint);
            #[cfg(windows)]
            let connection = std::fs::OpenOptions::new().write(true).open(&self.endpoint);
            if let Ok(mut connection) = connection {
                return writeln!(connection, "{command}")
                    .map_err(|e| format!("Ошибка управления mpv: {e}"));
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err("Не удалось подключиться к управлению mpv.".into())
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.endpoint);
            let _ = std::fs::remove_dir(&self.directory);
        }
    }
}
fn forward_output(stream: impl Read + Send + 'static, events: Sender<Event>) {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if !line.trim().is_empty() {
                let _ = events.send(Event::Message(line));
            }
        }
    });
}

#[derive(Clone, Default)]
pub struct State {
    pub queue: Vec<Track>,
    pub current: Option<Track>,
    pub paused: bool,
    pub loading: bool,
    pub volume: i32,
    pub can_previous: bool,
}
pub enum Action {
    Add(Vec<Track>),
    Play(Track),
    Remove(usize),
    Toggle,
    Next,
    Previous,
    Stop,
    Volume(i32),
    SetAuth(Auth),
    Quit,
}
pub enum Event {
    State(State),
    Message(String),
    Search(u64, Result<(Vec<SearchEntry>, String), String>),
    Cover(u64, Result<image::RgbImage, String>),
    Spectrum([f32; crate::spectrum::BANDS]),
    Lyrics(u64, Result<String, String>),
}
enum Resolved {
    Ready(u64, Track, Result<String, String>),
}

pub fn player_loop(rx: Receiver<Action>, events: Sender<Event>, mut auth: Auth) {
    let mut state = State {
        volume: 100,
        ..State::default()
    };
    let mut queue = VecDeque::new();
    let mut history = Vec::<Track>::new();
    let mut player: Option<Player> = None;
    let mut generation = 0u64;
    let mut last_spectrum = std::time::Instant::now();
    let (rtx, rrx) = mpsc::channel();
    let publish = |state: &mut State, queue: &VecDeque<Track>| {
        state.queue = queue.iter().cloned().collect();
        let _ = events.send(Event::State(state.clone()));
    };
    publish(&mut state, &queue);
    loop {
        if let Some(active) = player.as_mut() {
            match active.child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() && active.spectrum.is_some() {
                        let url = active.stream_url.clone();
                        let _ = events.send(Event::Spectrum([0.0; crate::spectrum::BANDS]));
                        let _ = events.send(Event::Message(
                            "Анализатор недоступен; повторяем без него".into(),
                        ));
                        match Player::start(&url, state.volume, events.clone(), false) {
                            Ok(active) => {
                                player = Some(active);
                                continue;
                            }
                            Err(e) => {
                                let _ = events
                                    .send(Event::Message(format!("Не удалось запустить mpv: {e}")));
                            }
                        }
                    }
                    if !status.success() {
                        let _ = events.send(Event::Message(format!(
                            "mpv завершился с ошибкой: {status}"
                        )));
                    }
                    if status.success() {
                        if let Some(track) = state.current.take() {
                            history.push(track);
                        }
                    }
                    state.can_previous = !history.is_empty();
                    player = None;
                    state.current = None;
                    state.paused = false;
                    publish(&mut state, &queue);
                }
                Err(e) => {
                    let _ = events.send(Event::Message(format!("Ошибка mpv: {e}")));
                    player = None;
                    state.current = None;
                    state.paused = false;
                    publish(&mut state, &queue);
                }
                _ => {}
            }
        }
        if last_spectrum.elapsed() >= Duration::from_millis(200) {
            let levels = if state.paused {
                [0.0; crate::spectrum::BANDS]
            } else {
                player
                    .as_ref()
                    .and_then(|p| p.spectrum.as_ref())
                    .map(|s| s.levels())
                    .unwrap_or([0.0; crate::spectrum::BANDS])
            };
            let _ = events.send(Event::Spectrum(levels));
            last_spectrum = std::time::Instant::now();
        }
        while let Ok(Resolved::Ready(id, track, result)) = rrx.try_recv() {
            if id != generation {
                continue;
            }
            state.loading = false;
            match result {
                Ok(url) => match Player::start(&url, state.volume, events.clone(), true) {
                    Ok(active) => {
                        player = Some(active);
                        state.current = Some(track);
                    }
                    Err(e) => {
                        state.current = None;
                        let _ =
                            events.send(Event::Message(format!("Не удалось запустить mpv: {e}")));
                    }
                },
                Err(e) => {
                    state.current = None;
                    let _ = events.send(Event::Message(format!("Не удалось получить поток: {e}")));
                }
            }
            publish(&mut state, &queue);
        }
        if player.is_none() && !state.loading {
            if let Some(track) = queue.pop_front() {
                generation = generation.wrapping_add(1);
                let id = generation;
                state.current = Some(track.clone());
                state.loading = true;
                state.paused = false;
                publish(&mut state, &queue);
                let tx = rtx.clone();
                let auth = auth.clone();
                thread::spawn(move || {
                    let result = resolve(&track, &auth);
                    let _ = tx.send(Resolved::Ready(id, track, result));
                });
            }
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Action::Add(tracks)) => queue.extend(tracks),
            Ok(Action::Play(track)) => {
                if player.is_some() {
                    if let Some(current) = state.current.take() {
                        history.push(current);
                    }
                }
                state.can_previous = !history.is_empty();
                generation = generation.wrapping_add(1);
                player = None;
                state.loading = false;
                state.current = None;
                state.paused = false;
                queue.clear();
                queue.push_back(track);
            }
            Ok(Action::Remove(index)) => {
                queue.remove(index);
            }
            Ok(Action::Toggle) => {
                if let Some(active) = &player {
                    match active.command("cycle pause") {
                        Ok(()) => state.paused = !state.paused,
                        Err(e) => {
                            let _ = events.send(Event::Message(e));
                        }
                    }
                }
            }
            Ok(Action::Next) => {
                if player.is_some() {
                    if let Some(current) = state.current.take() {
                        history.push(current);
                    }
                }
                state.can_previous = !history.is_empty();
                generation = generation.wrapping_add(1);
                player = None;
                state.loading = false;
                state.current = None;
                state.paused = false;
            }
            Ok(Action::Previous) => {
                if let Some(previous) = history.pop() {
                    if let Some(current) = state.current.take() {
                        queue.push_front(current);
                    }
                    queue.push_front(previous);
                    generation = generation.wrapping_add(1);
                    player = None;
                    state.loading = false;
                    state.paused = false;
                    state.can_previous = !history.is_empty();
                } else {
                    let _ = events.send(Event::Message("Предыдущего трека пока нет".into()));
                }
            }
            Ok(Action::Stop) => {
                history.clear();
                state.can_previous = false;
                generation = generation.wrapping_add(1);
                player = None;
                state.loading = false;
                state.current = None;
                state.paused = false;
                queue.clear();
            }
            Ok(Action::Volume(delta)) => {
                state.volume = (state.volume + delta).clamp(0, 100);
                if let Some(active) = &player {
                    if let Err(e) = active.command(&format!("set volume {}", state.volume)) {
                        let _ = events.send(Event::Message(e));
                    }
                }
            }
            Ok(Action::SetAuth(updated)) => auth = updated,
            Ok(Action::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
        }
        publish(&mut state, &queue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_channels_before_tracks_and_preserves_urls() {
        let results = parse_results("Youtube\tabc123\tArtist — Song\tabc123\nYoutubeTab\tUC123\tArtist\thttps://www.youtube.com/channel/UC123\n");
        assert!(
            matches!(&results[0], SearchEntry::Artist { title, url } if title == "Artist" && url.ends_with("UC123/videos"))
        );
        assert!(
            matches!(&results[1], SearchEntry::Track(track) if track.url == "https://www.youtube.com/watch?v=abc123")
        );
    }
}
