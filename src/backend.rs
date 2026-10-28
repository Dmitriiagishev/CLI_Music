use std::collections::VecDeque;
use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub url: String,
}

#[derive(Clone, Default, PartialEq, Eq)]
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
    let limit = limit.max(1);
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

static NEXT_PLAYER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioMode {
    Default,
    TvPipe,
    Pipe,
    HlsPipe,
}
// yt-dlp resolves the next stream in parallel. The pipe buffers its beginning
// and applies backpressure, so the whole track is not downloaded to disk.
struct Preload {
    url: String,
    auth: Auth,
    child: Option<Child>,
    stdout: Option<std::process::ChildStdout>,
    logs: Arc<Mutex<VecDeque<String>>>,
    reader: Option<thread::JoinHandle<()>>,
}
impl Preload {
    fn start(url: &str, auth: &Auth) -> io::Result<Self> {
        let mut cmd = Command::new("yt-dlp");
        auth.apply(&mut cmd);
        cmd.args([
            "--no-playlist",
            "--no-progress",
            "--extractor-args",
            "youtube:player_client=tv_samsung",
            "--http-chunk-size",
            "64K",
            "-f",
            "bestaudio/best",
            "-o",
            "-",
            "--",
            url,
        ]);
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let logs = Arc::new(Mutex::new(VecDeque::new()));
        let reader = child
            .stderr
            .take()
            .map(|stderr| collect_output(stderr, logs.clone()));
        let stdout = child.stdout.take();
        Ok(Self {
            url: url.into(),
            auth: auth.clone(),
            child: Some(child),
            stdout,
            logs,
            reader,
        })
    }
    fn failed(&mut self) -> bool {
        self.child
            .as_mut()
            .and_then(|p| p.try_wait().ok().flatten())
            .is_some_and(|status| !status.success())
    }
}
impl Drop for Preload {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
struct Player {
    child: Child,
    downloader: Option<Child>,
    endpoint: String,
    #[cfg(unix)]
    directory: std::path::PathBuf,
    spectrum: Option<crate::spectrum::Spectrum>,
    track_url: String,
    auth: Auth,
    mode: AudioMode,
    initial_error: Option<String>,
    logs: Arc<Mutex<VecDeque<String>>>,
    readers: Vec<thread::JoinHandle<()>>,
}

impl Player {
    fn start(
        url: &str,
        auth: &Auth,
        volume: i32,
        events: Sender<Event>,
        enable_spectrum: bool,
        mode: AudioMode,
        initial_error: Option<String>,
        prepared: Option<Preload>,
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
        let logs = prepared
            .as_ref()
            .map(|p| p.logs.clone())
            .unwrap_or_else(|| Arc::new(Mutex::new(VecDeque::new())));
        let mut readers = Vec::new();
        let mut downloader = None;
        let mut media_input = Stdio::null();
        let mut command = Command::new("mpv");
        if let Some(mut ready) = prepared {
            media_input = Stdio::from(
                ready
                    .stdout
                    .take()
                    .ok_or_else(|| io::Error::other("Поток предзагрузки недоступен"))?,
            );
            downloader = ready.child.take();
            if let Some(reader) = ready.reader.take() {
                readers.push(reader);
            }
            command.arg("--ytdl=no");
        } else if mode != AudioMode::Default {
            let mut extract = Command::new("yt-dlp");
            auth.apply(&mut extract);
            extract.args(["--no-playlist", "--no-progress"]);
            if mode == AudioMode::TvPipe {
                extract.args([
                    "--extractor-args",
                    "youtube:player_client=tv_samsung",
                    "--http-chunk-size",
                    "64K",
                    "-f",
                    "bestaudio/best",
                ]);
            } else if mode == AudioMode::HlsPipe {
                extract.args([
                    "--extractor-args",
                    "youtube:player_client=web_safari",
                    "-f",
                    "bestaudio[protocol^=m3u8]",
                ]);
            } else {
                // The authenticated Music client can use a PO-token provider.
                // Restrict this to Music URLs so direct YouTube links keep
                // yt-dlp's usual selection of clients.
                if authenticated_music(url, auth) {
                    extract.args(["--extractor-args", "youtube:player_client=web_music"]);
                }
                extract.args(["-f", "bestaudio/best"]);
            }
            extract.args(["-o", "-", "--", url]);
            let result = extract
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut process = match result {
                Ok(process) => process,
                Err(error) => {
                    #[cfg(unix)]
                    let _ = std::fs::remove_dir(&directory);
                    return Err(error);
                }
            };
            if let Some(stderr) = process.stderr.take() {
                readers.push(collect_output(stderr, logs.clone()));
            }
            if let Some(stdout) = process.stdout.take() {
                media_input = Stdio::from(stdout);
            }
            downloader = Some(process);
            command.arg("--ytdl=no");
        } else {
            command.args([
                "--ytdl=yes",
                "--script-opts=ytdl_hook-ytdl_path=yt-dlp",
                "--ytdl-format=bestaudio/best",
            ]);
            if let Some(browser) = &auth.browser {
                command.arg(format!(
                    "--ytdl-raw-options-append=cookies-from-browser={browser}"
                ));
            }
            if let Some(path) = &auth.cookies {
                command.arg(format!("--ytdl-raw-options-append=cookies={path}"));
            }
        }
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
                if mode != AudioMode::Default { "-" } else { url },
            ])
            .stdin(media_input)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match child {
            Ok(mut child) => {
                if let Some(stream) = child.stdout.take() {
                    readers.push(collect_output(stream, logs.clone()));
                }
                if let Some(stream) = child.stderr.take() {
                    readers.push(collect_output(stream, logs.clone()));
                }
                Ok(Self {
                    child,
                    downloader,
                    endpoint,
                    #[cfg(unix)]
                    directory,
                    spectrum,
                    track_url: url.to_owned(),
                    auth: auth.clone(),
                    mode,
                    initial_error,
                    logs,
                    readers,
                })
            }
            Err(e) => {
                if let Some(mut process) = downloader {
                    let _ = process.kill();
                    let _ = process.wait();
                }
                #[cfg(unix)]
                let _ = std::fs::remove_dir(&directory);
                Err(e)
            }
        }
    }

    fn finish_logs(&mut self) -> (String, bool) {
        let mut download_failed = false;
        if let Some(mut process) = self.downloader.take() {
            match process.try_wait() {
                Ok(Some(status)) => download_failed = !status.success(),
                _ => {
                    let _ = process.kill();
                    let _ = process.wait();
                }
            }
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        let output = self
            .logs
            .lock()
            .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        (output, download_failed)
    }

    #[cfg(unix)]
    fn playback_started(&self) -> bool {
        let Ok(mut socket) = std::os::unix::net::UnixStream::connect(&self.endpoint) else {
            return false;
        };
        let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
        if socket
            .write_all(b"{\"command\":[\"get_property\",\"time-pos\"]}\n")
            .is_err()
        {
            return false;
        }
        let mut response = String::new();
        BufReader::new(socket).read_line(&mut response).ok();
        serde_json::from_str::<serde_json::Value>(&response)
            .ok()
            .is_some_and(|value| value["error"] == "success" && value["data"].is_number())
    }
    #[cfg(windows)]
    fn playback_started(&self) -> bool {
        true
    }
    #[cfg(unix)]
    fn timing(&self) -> (Option<f64>, Option<f64>) {
        let Ok(mut socket) = std::os::unix::net::UnixStream::connect(&self.endpoint) else {
            return (None, None);
        };
        let _ = socket.set_read_timeout(Some(Duration::from_millis(40)));
        if socket.write_all(b"{\"command\":[\"get_property\",\"time-pos\"],\"request_id\":1}\n{\"command\":[\"get_property\",\"duration\"],\"request_id\":2}\n").is_err() { return (None, None); }
        let mut reader = BufReader::new(socket);
        let mut values = [None, None];
        for _ in 0..2 {
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                break;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                if value["error"] == "success" {
                    if let Some(id @ 1..=2) = value["request_id"].as_u64() {
                        values[(id - 1) as usize] = value["data"]
                            .as_f64()
                            .filter(|n| n.is_finite() && *n >= 0.0);
                    }
                }
            }
        }
        (values[0], values[1])
    }
    #[cfg(windows)]
    fn timing(&self) -> (Option<f64>, Option<f64>) {
        (None, None)
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
        if let Some(mut process) = self.downloader.take() {
            let _ = process.kill();
            let _ = process.wait();
        }
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.endpoint);
            let _ = std::fs::remove_dir(&self.directory);
        }
    }
}
fn sanitize_log(line: &str) -> String {
    let lowercase = line.to_ascii_lowercase();
    if lowercase.contains("cookie:") || lowercase.contains("authorization:") {
        return "[скрыты конфиденциальные заголовки]".into();
    }
    line.split_whitespace()
        .map(|word| {
            if word.contains("https://") || word.contains("http://") {
                "[ссылка скрыта]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect()
}
fn collect_output(
    stream: impl Read + Send + 'static,
    logs: Arc<Mutex<VecDeque<String>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(mut lines) = logs.lock() {
                if lines.len() >= 25 {
                    lines.pop_front();
                }
                lines.push_back(sanitize_log(&line));
            }
        }
    })
}
fn is_forbidden(logs: &str) -> bool {
    logs.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("403") && (line.contains("http") || line.contains("forbidden"))
    })
}
fn authenticated_music(url: &str, auth: &Auth) -> bool {
    url.starts_with("https://music.youtube.com/")
        && (auth.browser.is_some() || auth.cookies.is_some())
}
fn forbidden_retry(mode: AudioMode, logs: &str, music_auth: bool) -> Option<AudioMode> {
    let unavailable = logs
        .to_ascii_lowercase()
        .contains("requested format is not available");
    if mode != AudioMode::TvPipe && !is_forbidden(logs) && !(mode == AudioMode::Pipe && unavailable)
    {
        return None;
    }
    match mode {
        AudioMode::Default if music_auth => Some(AudioMode::TvPipe),
        AudioMode::Default => Some(AudioMode::Pipe),
        AudioMode::TvPipe => Some(AudioMode::Pipe),
        AudioMode::Pipe => Some(AudioMode::HlsPipe),
        AudioMode::HlsPipe => None,
    }
}
fn filter_failed(logs: &str) -> bool {
    logs.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("lavfi")
            || (line.contains("filter") && (line.contains("fail") || line.contains("error")))
    })
}

#[derive(Clone, Default)]
pub struct State {
    pub queue: Vec<Track>,
    pub current: Option<Track>,
    pub paused: bool,
    pub loading: bool,
    pub volume: i32,
    pub can_previous: bool,
    pub position: Option<f64>,
    pub duration: Option<f64>,
}
pub enum Action {
    Add(Vec<Track>),
    PlayList(Vec<Track>),
    PlayNext(Track),
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
    QueueTail(u64, Vec<Track>, Result<Vec<SearchEntry>, String>),
    Cover(u64, Result<image::RgbImage, String>),
    Spectrum([f32; crate::spectrum::BANDS]),
    Lyrics(u64, Result<String, String>),
}

pub fn player_loop(rx: Receiver<Action>, events: Sender<Event>, mut auth: Auth) {
    let mut state = State {
        volume: 100,
        ..State::default()
    };
    let mut queue = VecDeque::new();
    let mut history = Vec::<Track>::new();
    let mut player: Option<Player> = None;
    let mut preload: Option<Preload> = None;
    let mut last_spectrum = std::time::Instant::now();
    let mut last_progress = std::time::Instant::now();
    let publish = |state: &mut State, queue: &VecDeque<Track>| {
        state.queue = queue.iter().cloned().collect();
        let _ = events.send(Event::State(state.clone()));
    };
    publish(&mut state, &queue);
    loop {
        if let Some(active) = player.as_mut() {
            match active.child.try_wait() {
                Ok(Some(status)) => {
                    let (logs, download_failed) = active.finish_logs();
                    let success = status.success() && !download_failed;
                    if !success && active.spectrum.is_some() && filter_failed(&logs) {
                        let _ = events.send(Event::Spectrum([0.0; crate::spectrum::BANDS]));
                        let _ = events.send(Event::Message(
                            "Анализатор недоступен; повторяем без него".into(),
                        ));
                        match Player::start(
                            &active.track_url,
                            &active.auth,
                            state.volume,
                            events.clone(),
                            false,
                            active.mode,
                            active.initial_error.clone(),
                            None,
                        ) {
                            Ok(replacement) => {
                                state.loading = true;
                                publish(&mut state, &queue);
                                player = Some(replacement);
                                continue;
                            }
                            Err(e) => {
                                let _ = events.send(Event::Message(format!(
                                    "Не удалось запустить mpv: {e}\n{logs}"
                                )));
                            }
                        }
                    } else if let Some(next) = (!success)
                        .then(|| {
                            forbidden_retry(
                                active.mode,
                                &logs,
                                authenticated_music(&active.track_url, &active.auth),
                            )
                        })
                        .flatten()
                    {
                        let previous = if let Some(previous) = &active.initial_error {
                            format!("{previous}\nПовтор через yt-dlp:\n{logs}")
                        } else if active.mode == AudioMode::TvPipe {
                            logs.clone()
                        } else {
                            logs.lines()
                                .find(|line| is_forbidden(line))
                                .unwrap_or("HTTP 403 Forbidden")
                                .to_owned()
                        };
                        let message = match next {
                            AudioMode::TvPipe => "Поток вернул 403; пробуем клиент tv_samsung…",
                            AudioMode::Pipe => "Повторяем воспроизведение через yt-dlp…",
                            AudioMode::HlsPipe => {
                                "Повтор не удался; пробуем аудио HLS через yt-dlp…"
                            }
                            AudioMode::Default => unreachable!(),
                        };
                        let _ = events.send(Event::Message(message.into()));
                        match Player::start(
                            &active.track_url,
                            &active.auth,
                            state.volume,
                            events.clone(),
                            active.spectrum.is_some(),
                            next,
                            Some(previous),
                            None,
                        ) {
                            Ok(replacement) => {
                                state.loading = true;
                                publish(&mut state, &queue);
                                player = Some(replacement);
                                continue;
                            }
                            Err(e) => {
                                let _ = events.send(Event::Message(format!(
                                    "Не удалось повторить воспроизведение: {e}\n{logs}"
                                )));
                            }
                        }
                    } else if !success {
                        let previous = active
                            .initial_error
                            .as_ref()
                            .map(|error| format!("Первый поток: {error}\nПоследняя попытка:\n"))
                            .unwrap_or_default();
                        let outcome = if download_failed {
                            "yt-dlp завершился с ошибкой".to_owned()
                        } else {
                            format!("mpv: {status}")
                        };
                        let message = if logs.is_empty() {
                            format!("{previous}{outcome}")
                        } else {
                            format!("{previous}{outcome}\n{logs}")
                        };
                        let _ = events.send(Event::Message(message));
                    }
                    if success {
                        if let Some(track) = state.current.take() {
                            history.push(track);
                        }
                    }
                    state.can_previous = !history.is_empty();
                    player = None;
                    state.current = None;
                    state.position = None;
                    state.duration = None;
                    state.paused = false;
                    state.loading = false;
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
        if last_spectrum.elapsed() >= Duration::from_millis(100) {
            if state.loading && player.as_ref().is_some_and(|p| p.playback_started()) {
                state.loading = false;
                let _ = events.send(Event::Message(String::new()));
                publish(&mut state, &queue);
            }
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
        if last_progress.elapsed() >= Duration::from_millis(500) {
            if let Some(active) = &player {
                let (position, duration) = active.timing();
                state.position = position;
                state.duration = duration;
                publish(&mut state, &queue);
            }
            last_progress = std::time::Instant::now();
        }
        if player.is_none() && !state.loading {
            if let Some(track) = queue.pop_front() {
                let _ = events.send(Event::Message(String::new()));
                state.current = Some(track.clone());
                state.paused = false;
                state.loading = true;
                state.position = None;
                state.duration = None;
                let prepared = preload
                    .take()
                    .filter(|p| p.url == track.url && p.auth == auth)
                    .filter(|p| p.stdout.is_some());
                let prepared = prepared.and_then(|mut p| if p.failed() { None } else { Some(p) });
                match Player::start(
                    &track.url,
                    &auth,
                    state.volume,
                    events.clone(),
                    true,
                    if authenticated_music(&track.url, &auth) {
                        AudioMode::TvPipe
                    } else {
                        AudioMode::Default
                    },
                    None,
                    prepared,
                ) {
                    Ok(active) => player = Some(active),
                    Err(e) => {
                        state.current = None;
                        state.loading = false;
                        let _ =
                            events.send(Event::Message(format!("Не удалось запустить mpv: {e}")));
                    }
                }
                publish(&mut state, &queue);
            }
        }
        if player.is_some() {
            let next = queue
                .front()
                .filter(|track| authenticated_music(&track.url, &auth));
            if preload
                .as_ref()
                .is_some_and(|p| next.is_none_or(|t| p.url != t.url || p.auth != auth))
            {
                preload = None;
            }
            if preload.is_none() {
                if let Some(track) = next {
                    preload = Preload::start(&track.url, &auth).ok();
                }
            }
        } else if queue.is_empty() {
            preload = None;
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Action::Add(tracks)) => queue.extend(tracks),
            Ok(Action::PlayNext(track)) => queue.push_front(track),
            Ok(Action::PlayList(tracks)) => {
                if tracks.is_empty() {
                    continue;
                }
                if player.is_some() && !state.loading {
                    if let Some(current) = state.current.take() {
                        history.push(current);
                    }
                }
                state.can_previous = !history.is_empty();
                player = None;
                preload = None;
                state.loading = false;
                state.current = None;
                state.paused = false;
                queue = tracks.into();
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
                if player.is_some() && !state.loading {
                    if let Some(current) = state.current.take() {
                        history.push(current);
                    }
                }
                state.can_previous = !history.is_empty();
                player = None;
                state.loading = false;
                state.current = None;
                state.position = None;
                state.duration = None;
                state.paused = false;
            }
            Ok(Action::Previous) => {
                if let Some(previous) = history.pop() {
                    if let Some(current) = state.current.take() {
                        queue.push_front(current);
                    }
                    queue.push_front(previous);
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
                player = None;
                preload = None;
                state.loading = false;
                state.current = None;
                state.position = None;
                state.duration = None;
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
            Ok(Action::SetAuth(updated)) => {
                preload = None;
                auth = updated;
            }
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
    fn diagnostics_hide_sensitive_headers_and_stream_urls() {
        assert!(sanitize_log("Cookie: secret").contains("скрыты"));
        assert!(sanitize_log("Authorization: Bearer secret").contains("скрыты"));
        let output = sanitize_log("HTTP 403 https://example.org/stream?token=secret");
        assert!(output.contains("HTTP 403"));
        assert!(!output.contains("secret"));
    }

    #[test]
    fn retries_only_on_audio_filter_errors() {
        assert!(filter_failed("Failed to configure filter graph"));
        assert!(!filter_failed("Sign in to confirm you’re not a bot"));
        assert!(!filter_failed("HTTP 403 Forbidden"));
        assert!(is_forbidden("[ffmpeg] https: HTTP error 403 Forbidden"));
        assert!(is_forbidden("HTTP 403"));
        assert!(!is_forbidden("Sign in to confirm you're not a bot"));
    }

    #[test]
    fn stream_retries_are_bounded() {
        assert!(matches!(
            forbidden_retry(AudioMode::Default, "HTTP error 403", true),
            Some(AudioMode::TvPipe)
        ));
        assert!(matches!(
            forbidden_retry(AudioMode::Default, "HTTP error 403", false),
            Some(AudioMode::Pipe)
        ));
        assert!(matches!(
            forbidden_retry(AudioMode::TvPipe, "HTTP Error 403: Forbidden", true),
            Some(AudioMode::Pipe)
        ));
        assert!(matches!(
            forbidden_retry(AudioMode::TvPipe, "Sign in required", true),
            Some(AudioMode::Pipe)
        ));
        assert!(matches!(
            forbidden_retry(AudioMode::Pipe, "HTTP Error 403: Forbidden", true),
            Some(AudioMode::HlsPipe)
        ));
        assert!(forbidden_retry(AudioMode::HlsPipe, "HTTP 403", true).is_none());
        assert!(matches!(
            forbidden_retry(
                AudioMode::Pipe,
                "ERROR: Requested format is not available",
                true
            ),
            Some(AudioMode::HlsPipe)
        ));
        assert!(forbidden_retry(
            AudioMode::Pipe,
            "Sign in to confirm you are not a bot",
            true
        )
        .is_none());
    }

    #[test]
    fn music_retry_requires_music_url_and_cookies() {
        let auth = Auth {
            browser: Some("firefox".into()),
            cookies: None,
        };
        assert!(authenticated_music(
            "https://music.youtube.com/watch?v=abc",
            &auth
        ));
        assert!(!authenticated_music(
            "https://www.youtube.com/watch?v=abc",
            &auth
        ));
        assert!(!authenticated_music(
            "https://music.youtube.com/watch?v=abc",
            &Auth::default()
        ));
    }

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
