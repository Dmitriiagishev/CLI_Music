use std::collections::VecDeque;
use std::env;
use std::io::{self, BufRead, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug)]
struct Track {
    title: String,
    url: String,
}

#[derive(Clone, Default)]
struct Auth {
    browser: Option<String>,
    cookies: Option<String>,
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

fn lookup(query: &str, auth: &Auth) -> Result<Vec<Track>, String> {
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
    if !warnings.trim().is_empty() {
        eprintln!("{}", warnings.trim());
    }
    Ok(parse_tracks(&String::from_utf8_lossy(&output.stdout)))
}

enum Action {
    Add(Vec<Track>),
    Toggle,
    Next,
    Stop,
    Volume(i32),
    List,
    Quit,
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
    fn start(url: &str, volume: i32) -> io::Result<Self> {
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
            .spawn();
        match child {
            Ok(child) => Ok(Self {
                child,
                endpoint,
                #[cfg(unix)]
                directory,
            }),
            Err(e) => {
                #[cfg(unix)]
                let _ = std::fs::remove_dir(&directory);
                Err(e)
            }
        }
    }

    fn command(&self, command: &str) {
        // mpv creates the endpoint asynchronously after startup.
        for _ in 0..20 {
            #[cfg(unix)]
            let connection = std::os::unix::net::UnixStream::connect(&self.endpoint);
            #[cfg(windows)]
            let connection = std::fs::OpenOptions::new().write(true).open(&self.endpoint);
            if let Ok(mut connection) = connection {
                if let Err(e) = writeln!(connection, "{command}") {
                    eprintln!("Не удалось отправить команду mpv: {e}");
                }
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        eprintln!("Не удалось подключиться к управлению mpv.");
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
fn stop_player(player: &mut Option<Player>) {
    // Dropping the player terminates it and removes its private IPC directory.
    *player = None;
}
fn player_loop(rx: Receiver<Action>, auth: Auth) {
    let mut queue = VecDeque::<Track>::new();
    let mut player: Option<Player> = None;
    let mut current: Option<Track> = None;
    let mut volume = 100i32;
    loop {
        if let Some(active) = player.as_mut() {
            match active.child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        eprintln!("\nmpv завершился с ошибкой: {status}");
                    }
                    player = None;
                    current = None;
                }
                Err(e) => {
                    eprintln!("\nОшибка mpv: {e}");
                    stop_player(&mut player);
                    current = None;
                }
                _ => {}
            }
        }
        if player.is_none() {
            if let Some(track) = queue.pop_front() {
                println!("\n▶ {}", track.title);
                match resolve(&track, &auth) {
                    Ok(url) => match Player::start(&url, volume) {
                        Ok(active) => {
                            player = Some(active);
                            current = Some(track);
                        }
                        Err(e) => eprintln!("Не удалось запустить mpv: {e}"),
                    },
                    Err(e) => eprintln!("Не удалось получить поток: {e}"),
                }
                continue;
            }
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Action::Add(tracks)) => {
                println!("Добавлено: {}", tracks.len());
                queue.extend(tracks);
            }
            Ok(Action::Toggle) => {
                if let Some(active) = player.as_mut() {
                    active.command("cycle pause");
                } else {
                    println!("Сейчас ничего не играет.");
                }
            }
            Ok(Action::Next) => {
                stop_player(&mut player);
                current = None;
            }
            Ok(Action::Stop) => {
                stop_player(&mut player);
                current = None;
                queue.clear();
                println!("Воспроизведение остановлено, очередь очищена.");
            }
            Ok(Action::Volume(delta)) => {
                volume = (volume + delta).clamp(0, 100);
                if let Some(active) = player.as_mut() {
                    active.command(&format!("set volume {volume}"));
                }
                println!("Громкость: {volume}%");
            }
            Ok(Action::List) => {
                if let Some(track) = &current {
                    println!("Сейчас: {}", track.title);
                } else {
                    println!("Сейчас ничего не играет.");
                }
                if queue.is_empty() {
                    println!("Очередь пуста.");
                }
                for (i, track) in queue.iter().enumerate() {
                    println!("  {}. {}", i + 1, track.title);
                }
            }
            Ok(Action::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                stop_player(&mut player);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn help() {
    println!("Команды:\n  search <запрос> — найти музыку\n  add <номер> — добавить результат в очередь\n  play <номер> — остановить очередь и воспроизвести результат\n  url <ссылка> — добавить видео или плейлист\n  queue — показать очередь\n  pause — пауза / продолжить\n  next — следующий трек\n  stop — остановить и очистить очередь\n  volume <+/-число> — изменить громкость\n  help — справка\n  quit — выход");
}
fn send(tx: &Sender<Action>, action: Action) {
    let _ = tx.send(action);
}
fn main() {
    let mut auth = Auth::default();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--browser" => auth.browser = args.next(),
            "--cookies" => auth.cookies = args.next(),
            "--help" | "-h" => {
                println!("cli-music [--browser ИМЯ] [--cookies ПУТЬ]\n\nВход через cookies браузера или файл cookies в формате Netscape.\nНапример: cli-music --browser firefox");
                return;
            }
            _ => {
                eprintln!("Неизвестный аргумент: {arg}");
                std::process::exit(2);
            }
        }
        if (arg == "--browser" && auth.browser.is_none())
            || (arg == "--cookies" && auth.cookies.is_none())
        {
            eprintln!("Для {arg} нужно значение");
            std::process::exit(2);
        }
    }
    if auth.browser.is_some() && auth.cookies.is_some() {
        eprintln!("Выберите --browser или --cookies.");
        std::process::exit(2);
    }
    let (tx, rx) = mpsc::channel();
    let worker_auth = auth.clone();
    let worker = thread::spawn(move || player_loop(rx, worker_auth));
    let mut results: Vec<Track> = Vec::new();
    println!("CLI Music — поиск и прослушивание музыки. Введите help для списка команд.");
    let stdin = io::stdin();
    loop {
        print!("\n> ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            _ => {}
        }
        let line = line.trim();
        let (command, argument) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let argument = argument.trim();
        match command {
            "search" | "url" if !argument.is_empty() => {
                if command == "url"
                    && !(argument.starts_with("https://") || argument.starts_with("http://"))
                {
                    eprintln!("Введите ссылку http или https.");
                    continue;
                }
                println!("Загрузка…");
                match lookup(argument, &auth) {
                    Ok(tracks) if tracks.is_empty() => {
                        results.clear();
                        println!("Ничего не найдено. Проверьте запрос или сообщения yt-dlp выше.");
                    }
                    Ok(tracks) if command == "url" => send(&tx, Action::Add(tracks)),
                    Ok(tracks) => {
                        results = tracks;
                        for (i, t) in results.iter().enumerate() {
                            println!("{}. {}", i + 1, t.title);
                        }
                    }
                    Err(e) => eprintln!("{e}"),
                }
            }
            "add" | "play" => match argument
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|n| results.get(n))
            {
                Some(track) => {
                    if command == "play" {
                        send(&tx, Action::Stop);
                    }
                    send(&tx, Action::Add(vec![track.clone()]));
                }
                None => println!("Укажите номер из последнего поиска."),
            },
            "queue" => send(&tx, Action::List),
            "pause" => send(&tx, Action::Toggle),
            "next" => send(&tx, Action::Next),
            "stop" => send(&tx, Action::Stop),
            "volume" => match argument.parse::<i32>() {
                Ok(n) if (-100..=100).contains(&n) => send(&tx, Action::Volume(n)),
                _ => println!("Укажите изменение от -100 до +100, например volume -10."),
            },
            "help" => help(),
            "quit" | "exit" => break,
            "" => {}
            _ => println!("Неизвестная команда. Введите help."),
        }
    }
    send(&tx, Action::Quit);
    let _ = worker.join();
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
