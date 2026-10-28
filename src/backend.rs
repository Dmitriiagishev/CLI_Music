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

fn parse_tracks(output: &str) -> Vec<Track> {
    output
        .lines()
        .filter_map(|line| {
            let (id, title) = line.split_once('\t')?;
            if id.is_empty() || title.is_empty() {
                return None;
            }
            let url = if id.starts_with("https://") || id.starts_with("http://") {
                id.to_owned()
            } else {
                format!("https://www.youtube.com/watch?v={id}")
            };
            Some(Track {
                title: title.to_owned(),
                url,
            })
        })
        .collect()
}

pub fn lookup(query: &str, auth: &Auth) -> Result<(Vec<Track>, String), String> {
    let mut cmd = Command::new("yt-dlp");
    auth.apply(&mut cmd);
    let target = if query.starts_with("https://") || query.starts_with("http://") {
        query.to_owned()
    } else {
        format!("ytsearch10:{query}")
    };
    let output = cmd
        .args([
            "--ignore-config",
            "--flat-playlist",
            "--print",
            "%(id)s\t%(title)s",
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
        parse_tracks(&String::from_utf8_lossy(&output.stdout)),
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
}

impl Player {
    fn start(url: &str, volume: i32, events: Sender<Event>) -> io::Result<Self> {
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
        let child = Command::new("mpv")
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
}
pub enum Action {
    Add(Vec<Track>),
    Play(Track),
    Remove(usize),
    Toggle,
    Next,
    Stop,
    Volume(i32),
    Quit,
}
pub enum Event {
    State(State),
    Message(String),
    Search(u64, Result<(Vec<Track>, String), String>),
}
enum Resolved {
    Ready(u64, Track, Result<String, String>),
}

pub fn player_loop(rx: Receiver<Action>, events: Sender<Event>, auth: Auth) {
    let mut state = State {
        volume: 100,
        ..State::default()
    };
    let mut queue = VecDeque::new();
    let mut player: Option<Player> = None;
    let mut generation = 0u64;
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
                    if !status.success() {
                        let _ = events.send(Event::Message(format!(
                            "mpv завершился с ошибкой: {status}"
                        )));
                    }
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
        while let Ok(Resolved::Ready(id, track, result)) = rrx.try_recv() {
            if id != generation {
                continue;
            }
            state.loading = false;
            match result {
                Ok(url) => match Player::start(&url, state.volume, events.clone()) {
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
                generation = generation.wrapping_add(1);
                player = None;
                state.loading = false;
                state.current = None;
                state.paused = false;
            }
            Ok(Action::Stop) => {
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
    fn parses_search_results_and_preserves_titles() {
        let tracks = parse_tracks("abc123\tArtist — Song\ninvalid\nhttps://example.org/v\tOther\n");
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].url, "https://www.youtube.com/watch?v=abc123");
        assert_eq!(tracks[0].title, "Artist — Song");
        assert_eq!(tracks[1].url, "https://example.org/v");
    }
}
