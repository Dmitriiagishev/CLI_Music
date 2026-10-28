mod backend;
mod cover;

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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
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
    queue_open: bool,
    previous_focus: Focus,
    cover_url: Option<String>,
    cover_id: u64,
    cover: Option<image::RgbImage>,
    cover_status: String,
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
            queue_open: false,
            previous_focus: Focus::Search,
            cover_url: None,
            cover_id: 0,
            cover: None,
            cover_status: "Выберите трек".into(),
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
                let url = state.current.as_ref().map(|t| t.url.clone());
                if url != self.cover_url {
                    self.cover_url = url;
                    self.cover_id = self.cover_id.wrapping_add(1);
                    self.cover = None;
                    self.cover_status = if state.current.is_some() {
                        "Загрузка обложки…"
                    } else {
                        "Выберите трек"
                    }
                    .into();
                    if let Some(track) = state.current.clone() {
                        let id = self.cover_id;
                        let auth = self.auth.clone();
                        let events = self.events.clone();
                        thread::spawn(move || {
                            let result = cover::load(&track, &auth);
                            let _ = events.send(Event::Cover(id, result));
                        });
                    }
                }
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
            Event::Cover(id, result) if id == self.cover_id => match result {
                Ok(image) => {
                    self.cover = Some(image);
                    self.cover_status.clear();
                }
                Err(e) => self.cover_status = e,
            },
            Event::Cover(_, _) => {}
        }
    }
    fn toggle_queue(&mut self) {
        if self.queue_open {
            self.queue_open = false;
            self.focus = self.previous_focus;
        } else {
            self.queue_open = true;
            self.previous_focus = self.focus;
            self.focus = Focus::Queue;
        }
    }
    fn activate_button(&self) {
        match self.button {
            0 => self.send(Action::Previous),
            1 => self.send(Action::Toggle),
            2 => self.send(Action::Next),
            3 => self.send(Action::Stop),
            4 => self.send(Action::Volume(-10)),
            _ => self.send(Action::Volume(10)),
        }
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return false;
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.toggle_queue();
            return true;
        }
        if self.queue_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.toggle_queue(),
                KeyCode::Up => self.queue_selected = self.queue_selected.saturating_sub(1),
                KeyCode::Down => {
                    self.queue_selected =
                        (self.queue_selected + 1).min(self.state.queue.len().saturating_sub(1))
                }
                KeyCode::Delete | KeyCode::Char('d') if !self.state.queue.is_empty() => {
                    self.send(Action::Remove(self.queue_selected))
                }
                KeyCode::Char(' ') => self.send(Action::Toggle),
                KeyCode::Char('n') => self.send(Action::Next),
                KeyCode::Char('p') => self.send(Action::Previous),
                _ => {}
            }
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
                KeyCode::Down => {
                    self.focus = if self.results.is_empty() {
                        Focus::Controls
                    } else {
                        Focus::Results
                    }
                }
                _ => {}
            }
            return true;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Char('/') => self.focus = Focus::Search,
            KeyCode::Char(' ') if self.focus != Focus::Controls => self.send(Action::Toggle),
            KeyCode::Char('n') => self.send(Action::Next),
            KeyCode::Char('p') => self.send(Action::Previous),
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
            KeyCode::Up => match self.focus {
                Focus::Results if self.selected == 0 => self.focus = Focus::Search,
                Focus::Results => self.selected = self.selected.saturating_sub(1),
                Focus::Controls => {
                    self.focus = if self.results.is_empty() {
                        Focus::Search
                    } else {
                        Focus::Results
                    }
                }
                _ => {}
            },
            KeyCode::Down => match self.focus {
                Focus::Results if self.selected + 1 >= self.results.len() => {
                    self.focus = Focus::Controls
                }
                Focus::Results => self.selected += 1,
                _ => {}
            },
            KeyCode::Left if self.focus == Focus::Controls => {
                self.button = self.button.saturating_sub(1)
            }
            KeyCode::Right if self.focus == Focus::Controls => {
                self.button = (self.button + 1).min(5)
            }
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
    draw_cover(frame, columns[1], app);
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
    let labels = if rows[4].width < 75 {
        [
            "|◀",
            if app.state.paused { "▶" } else { "Ⅱ" },
            "▶|",
            "■",
            "−",
            "+",
        ]
    } else {
        [
            "⏮ Назад",
            if app.state.paused {
                "▶ Продолжить"
            } else {
                "Ⅱ Пауза"
            },
            "⏭ Далее",
            "■ Стоп",
            "− Тише",
            "+ Громче",
        ]
    };
    let buttons = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(16),
            Constraint::Percentage(24),
            Constraint::Percentage(16),
            Constraint::Percentage(15),
            Constraint::Percentage(14),
            Constraint::Percentage(15),
        ])
        .split(panel("", false).inner(rows[4]));
    frame.render_widget(
        panel(" Управление · ← → Enter ", app.focus == Focus::Controls),
        rows[4],
    );
    for (i, label) in labels.iter().enumerate() {
        let style = if app.focus == Focus::Controls && app.button == i {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        frame.render_widget(
            Paragraph::new(*label)
                .alignment(ratatui::layout::Alignment::Center)
                .style(style),
            buttons[i],
        );
    }
    let footer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(rows[5]);
    frame.render_widget(
        Paragraph::new(app.message.replace(['\n', '\r'], " "))
            .style(Style::default().fg(Color::Gray)),
        footer[0],
    );
    frame.render_widget(Paragraph::new("Tab: очередь  /: поиск  ↑↓: выбор  Enter: играть  a: добавить  Space: пауза  P/N: назад/далее  Q: выход").style(Style::default().fg(Color::Gray)), footer[1]);
    if app.queue_open {
        let popup = centered(area, 75, 70);
        frame.render_widget(Clear, popup);
        let queue: Vec<ListItem> = app
            .state
            .queue
            .iter()
            .map(|t| ListItem::new(t.title.as_str()))
            .collect();
        let mut selection = ListState::default();
        if !queue.is_empty() {
            selection.select(Some(app.queue_selected));
        }
        frame.render_stateful_widget(
            List::new(queue)
                .block(panel(
                    &format!(
                        " Очередь · {} · Tab/Esc закрыть · D удалить ",
                        app.state.queue.len()
                    ),
                    true,
                ))
                .highlight_style(highlight)
                .highlight_symbol("▸ "),
            popup,
            &mut selection,
        );
    }
}

fn centered(area: ratatui::layout::Rect, width: u16, height: u16) -> ratatui::layout::Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height) / 2),
            Constraint::Percentage(height),
            Constraint::Percentage((100 - height) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width) / 2),
            Constraint::Percentage(width),
            Constraint::Percentage((100 - width) / 2),
        ])
        .split(v[1])[1]
}
fn draw_cover(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let block = panel(" Обложка ", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
        frame.render_widget(
            Paragraph::new("Обложка доступна при включённых цветах терминала")
                .alignment(ratatui::layout::Alignment::Center),
            inner,
        );
        return;
    }
    let Some(image) = &app.cover else {
        frame.render_widget(
            Paragraph::new(app.cover_status.as_str()).alignment(ratatui::layout::Alignment::Center),
            inner,
        );
        return;
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let image = image::imageops::thumbnail(image, inner.width as u32, inner.height as u32 * 2);
    let x = inner.x + (inner.width - image.width() as u16) / 2;
    let rows = (image.height() as u16).div_ceil(2);
    let y = inner.y + (inner.height - rows) / 2;
    for row in 0..rows {
        let mut spans = Vec::new();
        for col in 0..image.width() {
            let top = image
                .get_pixel(col, (row as u32 * 2).min(image.height() - 1))
                .0;
            let bottom = image
                .get_pixel(col, (row as u32 * 2 + 1).min(image.height() - 1))
                .0;
            spans.push(Span::styled(
                "▀",
                Style::default()
                    .fg(Color::Rgb(top[0], top[1], top[2]))
                    .bg(Color::Rgb(bottom[0], bottom[1], bottom[2])),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            ratatui::layout::Rect::new(x, y + row, image.width() as u16, 1),
        );
    }
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
        app.button = 1;
        app.key(press(KeyCode::Enter));
        assert!(matches!(rx.try_recv(), Ok(Action::Toggle)));
        assert!(matches!(rx.try_recv(), Ok(Action::Toggle)));
    }

    #[test]
    fn queue_opens_on_tab_and_closes_without_losing_focus() {
        let (mut app, _) = make_app();
        app.focus = Focus::Results;
        app.key(press(KeyCode::Tab));
        assert!(app.queue_open);
        app.key(press(KeyCode::Esc));
        assert!(!app.queue_open && app.focus == Focus::Results);
    }

    #[test]
    fn arrows_reach_controls_from_search_without_queue() {
        let (mut app, _) = make_app();
        app.results = vec![Track {
            title: "one".into(),
            url: "one".into(),
        }];
        app.key(press(KeyCode::Down));
        assert!(app.focus == Focus::Results);
        app.key(press(KeyCode::Down));
        assert!(app.focus == Focus::Controls);
        assert!(!app.queue_open);
    }

    #[test]
    fn stale_cover_response_is_ignored() {
        let (mut app, _) = make_app();
        app.cover_id = 2;
        app.handle_event(Event::Cover(1, Ok(image::RgbImage::new(1, 1))));
        assert!(app.cover.is_none());
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
