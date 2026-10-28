mod backend;

use backend::{Action, Auth, Event, State, Track};
use crossterm::{
    event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::{
    env,
    io::{self, IsTerminal},
    sync::mpsc::{self, Sender},
    thread,
    time::Duration,
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Search,
    Results,
    Queue,
    Controls,
}
struct App {
    auth: Auth,
    actions: Sender<Action>,
    events: Sender<Event>,
    input: String,
    focus: Focus,
    results: Vec<Track>,
    selected: usize,
    queue_selected: usize,
    button: usize,
    state: State,
    message: String,
    search_id: u64,
    searching: bool,
}
impl App {
    fn new(auth: Auth, actions: Sender<Action>, events: Sender<Event>) -> Self {
        Self {
            auth,
            actions,
            events,
            input: String::new(),
            focus: Focus::Search,
            results: vec![],
            selected: 0,
            queue_selected: 0,
            button: 0,
            state: State::default(),
            message: "Введите запрос или ссылку и нажмите Enter".into(),
            search_id: 0,
            searching: false,
        }
    }
    fn send(&self, action: Action) {
        let _ = self.actions.send(action);
    }
    fn search(&mut self) {
        let query = self.input.trim().to_owned();
        if query.is_empty() {
            self.message = "Введите запрос или ссылку".into();
            return;
        }
        self.search_id = self.search_id.wrapping_add(1);
        let id = self.search_id;
        self.searching = true;
        self.message = "Ищем…".into();
        let auth = self.auth.clone();
        let events = self.events.clone();
        thread::spawn(move || {
            let result = backend::lookup(&query, &auth);
            let _ = events.send(Event::Search(id, result));
        });
    }
    fn handle_event(&mut self, event: Event) {
        match event {
            Event::State(state) => {
                self.state = state;
                self.queue_selected = self
                    .queue_selected
                    .min(self.state.queue.len().saturating_sub(1));
            }
            Event::Message(message) => self.message = message,
            Event::Search(id, result) if id == self.search_id => {
                self.searching = false;
                match result {
                    Ok((tracks, warning)) => {
                        self.results = tracks;
                        self.selected = 0;
                        self.message = if !warning.is_empty() {
                            warning
                        } else if self.results.is_empty() {
                            "Ничего не найдено".into()
                        } else {
                            format!(
                                "Найдено: {}. Enter — слушать, A — в очередь",
                                self.results.len()
                            )
                        };
                        if !self.results.is_empty() {
                            self.focus = Focus::Results;
                        }
                    }
                    Err(error) => {
                        self.results.clear();
                        self.message = error;
                    }
                }
            }
            Event::Search(_, _) => {}
        }
    }
    fn cycle_focus(&mut self, reverse: bool) {
        self.focus = match (self.focus, reverse) {
            (Focus::Search, false) | (Focus::Queue, true) => Focus::Results,
            (Focus::Results, false) | (Focus::Controls, true) => Focus::Queue,
            (Focus::Queue, false) | (Focus::Search, true) => Focus::Controls,
            _ => Focus::Search,
        };
    }
    fn activate_button(&self) {
        match self.button {
            0 => self.send(Action::Toggle),
            1 => self.send(Action::Next),
            2 => self.send(Action::Stop),
            3 => self.send(Action::Volume(-10)),
            _ => self.send(Action::Volume(10)),
        }
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return false;
        }
        if key.code == KeyCode::Tab {
            self.cycle_focus(false);
            return true;
        }
        if key.code == KeyCode::BackTab {
            self.cycle_focus(true);
            return true;
        }
        if self.focus == Focus::Search {
            match key.code {
                KeyCode::Esc => self.focus = Focus::Results,
                KeyCode::Enter => self.search(),
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.input.push(c)
                }
                KeyCode::Down => self.focus = Focus::Results,
                _ => {}
            }
            return true;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Char('/') => self.focus = Focus::Search,
            KeyCode::Char(' ') if self.focus != Focus::Controls => self.send(Action::Toggle),
            KeyCode::Char('n') => self.send(Action::Next),
            KeyCode::Char('+') | KeyCode::Char('=') => self.send(Action::Volume(10)),
            KeyCode::Char('-') => self.send(Action::Volume(-10)),
            KeyCode::Char('a') if self.focus == Focus::Results => {
                if let Some(track) = self.results.get(self.selected) {
                    self.send(Action::Add(vec![track.clone()]));
                    self.message = format!("Добавлено: {}", track.title);
                }
            }
            KeyCode::Char('A') if self.focus == Focus::Results => {
                if !self.results.is_empty() {
                    self.send(Action::Add(self.results.clone()));
                    self.message = format!("Добавлено в очередь: {}", self.results.len());
                }
            }
            KeyCode::Delete | KeyCode::Char('d') if self.focus == Focus::Queue => {
                if !self.state.queue.is_empty() {
                    self.send(Action::Remove(self.queue_selected));
                }
            }
            KeyCode::Up => match self.focus {
                Focus::Results if self.selected == 0 => self.focus = Focus::Search,
                Focus::Results => self.selected = self.selected.saturating_sub(1),
                Focus::Queue => self.queue_selected = self.queue_selected.saturating_sub(1),
                Focus::Controls => self.focus = Focus::Queue,
                _ => {}
            },
            KeyCode::Down => match self.focus {
                Focus::Results => {
                    self.selected = (self.selected + 1).min(self.results.len().saturating_sub(1))
                }
                Focus::Queue => {
                    if self.queue_selected + 1 >= self.state.queue.len() {
                        self.focus = Focus::Controls;
                    } else {
                        self.queue_selected += 1;
                    }
                }
                Focus::Controls => {}
                _ => {}
            },
            KeyCode::Left => match self.focus {
                Focus::Queue => self.focus = Focus::Results,
                Focus::Controls => self.button = self.button.saturating_sub(1),
                _ => {}
            },
            KeyCode::Right => match self.focus {
                Focus::Results => self.focus = Focus::Queue,
                Focus::Controls => self.button = (self.button + 1).min(4),
                _ => {}
            },
            KeyCode::Enter => match self.focus {
                Focus::Results => {
                    if let Some(track) = self.results.get(self.selected) {
                        self.send(Action::Play(track.clone()));
                    }
                }
                Focus::Controls => self.activate_button(),
                _ => {}
            },
            KeyCode::Char(' ') if self.focus == Focus::Controls => self.activate_button(),
            _ => {}
        }
        true
    }
}

fn panel(title: &str, selected: bool) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if selected {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
}
fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 45 || area.height < 18 {
        frame.render_widget(
            Paragraph::new("Увеличьте окно терминала (минимум 45 × 18)"),
            area,
        );
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .split(area);
    let header = Line::from(vec![
        Span::styled(
            "  CLI MUSIC  ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("   музыка в терминале", Style::default().fg(Color::Gray)),
    ]);
    frame.render_widget(Paragraph::new(header).block(panel("", false)), rows[0]);
    let search = if app.input.is_empty() {
        "Введите название, исполнителя или ссылку…"
    } else {
        &app.input
    };
    let visible_width = rows[1].width.saturating_sub(2) as usize;
    let input_width = UnicodeWidthStr::width(app.input.as_str());
    let scroll = input_width.saturating_sub(visible_width.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(search)
            .scroll((0, scroll.min(u16::MAX as usize) as u16))
            .style(Style::default().fg(if app.input.is_empty() {
                Color::DarkGray
            } else {
                Color::White
            }))
            .block(panel(
                if app.searching {
                    " Поиск · загрузка… "
                } else {
                    " Поиск · Enter "
                },
                app.focus == Focus::Search,
            )),
        rows[1],
    );
    if app.focus == Focus::Search && visible_width > 0 {
        frame.set_cursor_position((rows[1].x + 1 + (input_width - scroll) as u16, rows[1].y + 1));
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rows[2]);
    let highlight = Style::default()
        .bg(Color::DarkGray)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let results: Vec<ListItem> = app
        .results
        .iter()
        .map(|t| ListItem::new(t.title.as_str()))
        .collect();
    let mut rs = ListState::default();
    if !results.is_empty() {
        rs.select(Some(app.selected));
    }
    frame.render_stateful_widget(
        List::new(results)
            .block(panel(
                &format!(" Результаты · {} ", app.results.len()),
                app.focus == Focus::Results,
            ))
            .highlight_style(highlight)
            .highlight_symbol("▸ "),
        columns[0],
        &mut rs,
    );
    let queue: Vec<ListItem> = app
        .state
        .queue
        .iter()
        .map(|t| ListItem::new(t.title.as_str()))
        .collect();
    let mut qs = ListState::default();
    if !queue.is_empty() {
        qs.select(Some(app.queue_selected));
    }
    frame.render_stateful_widget(
        List::new(queue)
            .block(panel(
                &format!(" Очередь · {} ", app.state.queue.len()),
                app.focus == Focus::Queue,
            ))
            .highlight_style(highlight)
            .highlight_symbol("▸ "),
        columns[1],
        &mut qs,
    );
    let status = if app.state.loading {
        "Загрузка"
    } else if app.state.paused {
        "Пауза"
    } else if app.state.current.is_some() {
        "Играет"
    } else {
        "Остановлено"
    };
    let title = app
        .state
        .current
        .as_ref()
        .map(|t| t.title.as_str())
        .unwrap_or("Выберите трек");
    frame.render_widget(
        Paragraph::new(format!(
            " {status}  ·  {title}  ·  Громкость {}%",
            app.state.volume
        ))
        .block(panel(" Сейчас ", false)),
        rows[3],
    );
    let labels = [
        if app.state.paused {
            "▶ Продолжить"
        } else {
            "Ⅱ Пауза"
        },
        "⏭ Далее",
        "■ Стоп",
        "− Тише",
        "+ Громче",
    ];
    let spans: Vec<Span> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            Span::styled(
                format!(" {label} "),
                if app.focus == Focus::Controls && app.button == i {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                },
            )
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(panel(
            " Управление · ← → Enter ",
            app.focus == Focus::Controls,
        )),
        rows[4],
    );
    let footer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(rows[5]);
    frame.render_widget(
        Paragraph::new(app.message.replace(['\n', '\r'], " "))
            .style(Style::default().fg(Color::Gray)),
        footer[0],
    );
    frame.render_widget(Paragraph::new("Tab: панели  /: поиск  ↑↓: выбор  Enter: играть  A: очередь  Space: пауза  N: далее  Q: выход").style(Style::default().fg(Color::Gray)), footer[1]);
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}
fn run(auth: Auth) -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "Для интерфейса нужен интерактивный терминал",
        ));
    }
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let (actions, action_rx) = mpsc::channel();
    let (events, event_rx) = mpsc::channel();
    let worker_auth = auth.clone();
    let worker_events = events.clone();
    let worker = thread::spawn(move || backend::player_loop(action_rx, worker_events, worker_auth));
    let mut app = App::new(auth, actions, events);
    let result = (|| -> io::Result<()> {
        loop {
            while let Ok(event) = event_rx.try_recv() {
                app.handle_event(event);
            }
            terminal.draw(|frame| draw(frame, &app))?;
            if event::poll(Duration::from_millis(100))? {
                if let TermEvent::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Release && !app.key(key) {
                        break;
                    }
                }
            }
        }
        Ok(())
    })();
    app.send(Action::Quit);
    let _ = worker.join();
    result
}
fn main() {
    let mut auth = Auth::default();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--browser" => auth.browser = args.next(),
            "--cookies" => auth.cookies = args.next(),
            "--help" | "-h" => {
                println!("cli-music [--browser ИМЯ] [--cookies ПУТЬ]\nВход через cookies браузера или файл cookies в формате Netscape.");
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
    if let Err(e) = run(auth) {
        eprintln!("Ошибка: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app() -> (App, mpsc::Receiver<Action>) {
        let (actions, rx) = mpsc::channel();
        let (events, _) = mpsc::channel();
        (App::new(Auth::default(), actions, events), rx)
    }
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn arrows_select_track_and_enter_plays_it() {
        let (mut app, rx) = make_app();
        app.results = vec![
            Track {
                title: "one".into(),
                url: "one".into(),
            },
            Track {
                title: "two".into(),
                url: "two".into(),
            },
        ];
        app.key(press(KeyCode::Down));
        app.key(press(KeyCode::Down));
        app.key(press(KeyCode::Enter));
        match rx.try_recv().unwrap() {
            Action::Play(track) => assert_eq!(track.title, "two"),
            _ => panic!("expected playback"),
        }
    }

    #[test]
    fn space_and_control_button_both_toggle_pause() {
        let (mut app, rx) = make_app();
        app.focus = Focus::Results;
        app.key(press(KeyCode::Char(' ')));
        app.focus = Focus::Controls;
        app.key(press(KeyCode::Enter));
        assert!(matches!(rx.try_recv(), Ok(Action::Toggle)));
        assert!(matches!(rx.try_recv(), Ok(Action::Toggle)));
    }

    #[test]
    fn older_search_response_cannot_replace_new_results() {
        let (mut app, _) = make_app();
        app.search_id = 2;
        app.handle_event(Event::Search(
            2,
            Ok((
                vec![Track {
                    title: "new".into(),
                    url: "url".into(),
                }],
                String::new(),
            )),
        ));
        app.handle_event(Event::Search(
            1,
            Ok((
                vec![Track {
                    title: "old".into(),
                    url: "url".into(),
                }],
                String::new(),
            )),
        ));
        assert_eq!(app.results[0].title, "new");
    }
}
