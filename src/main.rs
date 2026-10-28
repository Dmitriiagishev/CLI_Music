mod backend;
mod cover;
mod music;
mod spectrum;

#[cfg(test)]
use backend::Track;
use backend::{Action, Auth, Event, SearchEntry, State};
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
    results: Vec<SearchEntry>,
    result_limit: usize,
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
    levels: [f32; spectrum::BANDS],
    lyrics: String,
    lyrics_open: bool,
    lyrics_scroll: u16,
    auth_open: bool,
    auth_selection: usize,
    details_open: bool,
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
            result_limit: 10,
            selected: 0,
            queue_selected: 0,
            button: 0,
            state: State::default(),
            message: String::new(),
            search_id: 0,
            searching: false,
            queue_open: false,
            previous_focus: Focus::Search,
            cover_url: None,
            cover_id: 0,
            cover: None,
            cover_status: "Выберите трек".into(),
            levels: [0.0; spectrum::BANDS],
            lyrics: "Выберите трек".into(),
            lyrics_open: false,
            lyrics_scroll: 0,
            auth_open: false,
            auth_selection: 0,
            details_open: false,
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
        self.search_query(query);
    }
    fn search_query(&mut self, query: String) {
        self.search_id = self.search_id.wrapping_add(1);
        let id = self.search_id;
        self.searching = true;
        self.message = "Ищем…".into();
        let auth = self.auth.clone();
        let events = self.events.clone();
        let limit = self.result_limit;
        thread::spawn(move || {
            let result = backend::lookup(&query, &auth, limit);
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
                    self.lyrics = if state.current.is_some() {
                        "Загружаем текст…"
                    } else {
                        "Выберите трек"
                    }
                    .into();
                    self.lyrics_scroll = 0;
                    if let Some(track) = state.current.clone() {
                        let id = self.cover_id;
                        let events = self.events.clone();
                        thread::spawn(move || {
                            let result = music::lyrics(&track.url);
                            let _ = events.send(Event::Lyrics(id, result));
                        });
                    }
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
                            format!("Найдено: {}", self.results.len())
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
            Event::Spectrum(levels) => self.levels = levels,
            Event::Lyrics(id, result) if id == self.cover_id => {
                self.lyrics = result.unwrap_or_else(|e| format!("Текст недоступен: {e}"))
            }
            Event::Lyrics(_, _) => {}
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
            _ => self.send(Action::Next),
        }
    }
    fn choose_browser(&mut self) {
        let browsers = ["firefox", "chrome", "chromium", "brave", "edge"];
        let browser = browsers[self.auth_selection];
        self.auth = Auth {
            browser: Some(browser.to_owned()),
            cookies: None,
        };
        self.send(Action::SetAuth(self.auth.clone()));
        self.auth_open = false;
        self.message = match save_browser(browser) {
            Ok(()) => {
                format!("Выбран {browser}. Войдите в YouTube в браузере и запустите трек снова.")
            }
            Err(e) => format!("Браузер выбран на этот запуск; сохранить выбор не удалось: {e}"),
        };
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return false;
        }
        if self.details_open {
            self.details_open = false;
            return true;
        }
        if self.auth_open {
            match key.code {
                KeyCode::Esc => self.auth_open = false,
                KeyCode::Up => self.auth_selection = self.auth_selection.saturating_sub(1),
                KeyCode::Down => self.auth_selection = (self.auth_selection + 1).min(4),
                KeyCode::Enter => self.choose_browser(),
                _ => {}
            }
            return true;
        }
        if self.lyrics_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('l') | KeyCode::Char('q') => self.lyrics_open = false,
                KeyCode::Up => self.lyrics_scroll = self.lyrics_scroll.saturating_sub(1),
                KeyCode::Down => self.lyrics_scroll = self.lyrics_scroll.saturating_add(1),
                _ => {}
            }
            return true;
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
            KeyCode::Char('g') => self.auth_open = true,
            KeyCode::Char('l') => self.lyrics_open = true,
            KeyCode::Char('e') => self.details_open = true,
            KeyCode::Char('s') => self.send(Action::Stop),
            KeyCode::Char(' ') if self.focus != Focus::Controls => self.send(Action::Toggle),
            KeyCode::Char('n') => self.send(Action::Next),
            KeyCode::Char('p') => self.send(Action::Previous),
            KeyCode::Char('+') | KeyCode::Char('=') => self.send(Action::Volume(10)),
            KeyCode::Char('-') => self.send(Action::Volume(-10)),
            KeyCode::Char('a') if self.focus == Focus::Results => {
                if let Some(SearchEntry::Track(track)) = self.results.get(self.selected) {
                    self.send(Action::Add(vec![track.clone()]));
                    self.message = format!("Добавлено: {}", track.title);
                }
            }
            KeyCode::Char('A') if self.focus == Focus::Results => {
                let tracks: Vec<_> = self
                    .results
                    .iter()
                    .filter_map(|entry| match entry {
                        SearchEntry::Track(track) => Some(track.clone()),
                        _ => None,
                    })
                    .collect();
                if !tracks.is_empty() {
                    self.message = format!("Добавлено в очередь: {}", tracks.len());
                    self.send(Action::Add(tracks));
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
                self.button = (self.button + 1).min(2)
            }
            KeyCode::Enter => match self.focus {
                Focus::Results => match self.results.get(self.selected).cloned() {
                    Some(SearchEntry::Track(track)) => self.send(Action::Play(track)),
                    Some(SearchEntry::Artist { url, .. }) => self.search_query(url),
                    None => {}
                },
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
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(8),
        ])
        .split(columns[0]);
    let search = if app.input.is_empty() {
        "Введите название, исполнителя или ссылку…"
    } else {
        &app.input
    };
    let visible_width = left[0].width.saturating_sub(2) as usize;
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
        left[0],
    );
    if app.focus == Focus::Search
        && visible_width > 0
        && !app.queue_open
        && !app.auth_open
        && !app.lyrics_open
        && !app.details_open
    {
        frame.set_cursor_position((left[0].x + 1 + (input_width - scroll) as u16, left[0].y + 1));
    }

    let highlight = Style::default()
        .bg(Color::DarkGray)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let artists = app
        .results
        .iter()
        .filter(|e| matches!(e, SearchEntry::Artist { .. }))
        .count();
    let tracks = app.results.len() - artists;
    let mut results = Vec::new();
    if artists > 0 {
        results.push(
            ListItem::new(" ИСПОЛНИТЕЛИ").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    }
    for entry in app.results.iter().take(artists) {
        results.push(ListItem::new(entry.title()));
    }
    if tracks > 0 {
        results.push(
            ListItem::new(" ТРЕКИ").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    }
    for entry in app.results.iter().skip(artists) {
        results.push(ListItem::new(entry.title()));
    }
    let mut rs = ListState::default();
    if !app.results.is_empty() {
        let header_count =
            usize::from(artists > 0) + usize::from(tracks > 0 && app.selected >= artists);
        rs.select(Some(app.selected + header_count));
    }
    frame.render_stateful_widget(
        List::new(results)
            .block(panel(
                &format!(" Результаты · {} ", app.results.len()),
                app.focus == Focus::Results,
            ))
            .highlight_style(highlight)
            .highlight_symbol("▸ "),
        left[1],
        &mut rs,
    );
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(8)])
        .split(columns[1]);
    draw_cover(frame, right[0], app);
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
    let player_block = panel(
        " Плеер · G вход · L текст · E сообщение ",
        app.focus == Focus::Controls,
    );
    let inside = player_block.inner(left[2]);
    frame.render_widget(player_block, left[2]);
    let player_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inside);
    frame.render_widget(
        Paragraph::new(title)
            .alignment(ratatui::layout::Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        player_rows[0],
    );
    frame.render_widget(
        Paragraph::new(format!("{status} · {}%", app.state.volume))
            .alignment(ratatui::layout::Alignment::Center)
            .style(Style::default().fg(Color::Gray)),
        player_rows[1],
    );
    let labels = ["⏮", if app.state.paused { "▶" } else { "Ⅱ" }, "⏭"];
    let buttons = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 3); 3])
        .split(player_rows[2]);
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
    frame.render_widget(
        Paragraph::new(app.message.replace(['\n', '\r'], " "))
            .style(Style::default().fg(Color::Gray)),
        player_rows[3],
    );
    draw_spectrum(frame, right[1], app);
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
    if app.auth_open {
        let popup = centered(area, 70, 70);
        frame.render_widget(Clear, popup);
        let browsers = ["Firefox", "Chrome", "Chromium", "Brave", "Edge"];
        let mut selection = ListState::default();
        selection.select(Some(app.auth_selection));
        let items: Vec<_> = browsers.iter().map(|name| ListItem::new(*name)).collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(
                    " Войдите в YouTube в браузере · выберите его · Enter ",
                    true,
                ))
                .highlight_style(highlight)
                .highlight_symbol("▸ "),
            popup,
            &mut selection,
        );
    }
    if app.details_open {
        let popup = centered(area, 75, 50);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(app.message.as_str())
                .wrap(ratatui::widgets::Wrap { trim: false })
                .block(panel(" Сообщение · любая клавиша закрывает ", true)),
            popup,
        );
    }
    if app.lyrics_open {
        let popup = centered(area, 75, 80);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(app.lyrics.as_str())
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((app.lyrics_scroll, 0))
                .block(panel(" Текст · ↑↓ прокрутка · Esc закрыть ", true)),
            popup,
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
    let block = panel("", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let has_color = !env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    let image_rows = if app.cover.is_some() && has_color {
        inner.height.saturating_sub(3).min(11)
    } else {
        1
    };
    if let Some(image) = app.cover.as_ref().filter(|_| has_color && image_rows > 0) {
        let side = inner.width.min(image_rows.saturating_mul(2)).min(22) as u32;
        if side > 0 {
            let image = image::imageops::thumbnail(image, side, side);
            let x = inner.x + (inner.width - image.width() as u16) / 2;
            let rows = (image.height() as u16).div_ceil(2);
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
                    ratatui::layout::Rect::new(x, inner.y + row, image.width() as u16, 1),
                );
            }
        }
    } else if image_rows > 0 {
        let text = if !has_color && app.cover.is_some() {
            "Для обложки включите цвета терминала"
        } else {
            app.cover_status.as_str()
        };
        frame.render_widget(
            Paragraph::new(text).alignment(ratatui::layout::Alignment::Center),
            ratatui::layout::Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }
    let offset = (image_rows + 1).min(inner.height);
    let text_area = ratatui::layout::Rect::new(
        inner.x,
        inner.y + offset,
        inner.width,
        inner.height - offset,
    );
    if text_area.height > 0 && app.state.current.is_some() {
        frame.render_widget(
            Paragraph::new(app.lyrics.as_str())
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((app.lyrics_scroll, 0))
                .alignment(ratatui::layout::Alignment::Center)
                .style(Style::default().fg(Color::Gray)),
            text_area,
        );
    }
}

fn draw_spectrum(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let block = panel("", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < spectrum::BANDS as u16 || inner.height == 0 {
        return;
    }
    let labels = inner.height >= 3 && inner.width >= 32;
    let height = inner.height.saturating_sub(u16::from(labels));
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 16); spectrum::BANDS])
        .split(inner);
    let frequency = [
        "63", "", "125", "", "250", "", "500", "", "1k", "", "2k", "", "4k", "", "8k", "",
    ];
    for (index, column) in columns.iter().enumerate() {
        let lit = (app.levels[index] * f32::from(height)).round() as u16;
        for row in 0..height {
            let bottom = height - row;
            if bottom > lit {
                continue;
            }
            let fraction = f32::from(bottom) / f32::from(height);
            let color = spectrum_color(fraction);
            let segment = "─".repeat(usize::from(column.width.saturating_sub(1).max(1)));
            frame.render_widget(
                Paragraph::new(segment)
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(Style::default().fg(color)),
                ratatui::layout::Rect::new(column.x, inner.y + row, column.width, 1),
            );
        }
        if labels {
            frame.render_widget(
                Paragraph::new(frequency[index])
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(Style::default().fg(Color::Gray)),
                ratatui::layout::Rect::new(column.x, inner.y + height, column.width, 1),
            );
        }
    }
}

fn spectrum_color(fraction: f32) -> Color {
    if fraction <= 0.5 {
        Color::Reset
    } else if fraction <= 0.8 {
        Color::Rgb(216, 139, 72)
    } else {
        Color::Rgb(121, 44, 67)
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
            app.result_limit =
                usize::from(terminal.size()?.height.saturating_sub(15)).clamp(1, 100);
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
fn browser_path() -> Option<std::path::PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(|home| {
            std::path::PathBuf::from(home)
                .join(".cli-music")
                .join("browser")
        })
}
fn load_browser() -> Option<String> {
    let name = std::fs::read_to_string(browser_path()?)
        .ok()?
        .trim()
        .to_owned();
    ["firefox", "chrome", "chromium", "brave", "edge"]
        .contains(&name.as_str())
        .then_some(name)
}
fn save_browser(name: &str) -> io::Result<()> {
    use std::io::Write;
    let path = browser_path().ok_or_else(|| io::Error::other("Домашняя папка недоступна"))?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Каталог настроек недоступен"))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(name.as_bytes())?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
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
    if auth.browser.is_none() && auth.cookies.is_none() {
        auth.browser = load_browser();
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
            SearchEntry::Track(Track {
                title: "one".into(),
                url: "one".into(),
            }),
            SearchEntry::Track(Track {
                title: "two".into(),
                url: "two".into(),
            }),
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
        app.results = vec![SearchEntry::Track(Track {
            title: "one".into(),
            url: "one".into(),
        })];
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
    fn spectrum_uses_theme_color_then_orange_and_burgundy() {
        assert_eq!(spectrum::BANDS, 16);
        assert_eq!(spectrum_color(0.5), Color::Reset);
        assert_eq!(spectrum_color(0.65), Color::Rgb(216, 139, 72));
        assert_eq!(spectrum_color(0.9), Color::Rgb(121, 44, 67));
    }
    #[test]
    fn browser_picker_moves_selection() {
        let (mut app, rx) = make_app();
        app.focus = Focus::Results;
        app.key(press(KeyCode::Char('g')));
        assert!(app.auth_open);
        app.key(press(KeyCode::Down));
        // Selection is passed to the player; no credentials are copied.
        // Avoid saving to a user's home in a unit test by checking the picker before Enter.
        assert_eq!(app.auth_selection, 1);
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn layout_places_lyrics_right_and_player_below_results() {
        let (mut app, _) = make_app();
        app.state.current = Some(Track {
            title: "Example".into(),
            url: "url".into(),
        });
        app.lyrics = "First lyric".into();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let lines: Vec<String> = (0..40)
            .map(|y| {
                (0..120)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect()
            })
            .collect();
        let (lyric_row, lyric_text) = lines
            .iter()
            .enumerate()
            .find(|(_, line)| line.contains("First lyric"))
            .unwrap();
        let (player_row, player_text) = lines
            .iter()
            .enumerate()
            .find(|(_, line)| line.contains("Плеер"))
            .unwrap();
        let (results_row, _) = lines
            .iter()
            .enumerate()
            .find(|(_, line)| line.contains("Результаты"))
            .unwrap();
        assert!(lyric_text.find("First lyric").unwrap() >= 72);
        assert!(player_text.find("Плеер").unwrap() < 72);
        assert!(player_row > lyric_row && player_row > results_row);
        assert!(!lines
            .iter()
            .any(|line| line.contains("Обложка") || line.contains("Частоты")));
    }

    #[test]
    fn older_search_response_cannot_replace_new_results() {
        let (mut app, _) = make_app();
        app.search_id = 2;
        app.handle_event(Event::Search(
            2,
            Ok((
                vec![SearchEntry::Track(Track {
                    title: "new".into(),
                    url: "url".into(),
                })],
                String::new(),
            )),
        ));
        app.handle_event(Event::Search(
            1,
            Ok((
                vec![SearchEntry::Track(Track {
                    title: "old".into(),
                    url: "url".into(),
                })],
                String::new(),
            )),
        ));
        assert_eq!(app.results[0].title(), "new");
    }
}
