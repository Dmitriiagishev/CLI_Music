mod backend;
mod cover;
mod music;
mod spectrum;

use backend::Track;
use backend::{Action, Auth, Event, SearchEntry, State};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event as TermEvent, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Search,
    Results,
    Queue,
    Controls,
    Related,
}
struct ContextMenu {
    track: Track,
    choice: usize,
    x: u16,
    y: u16,
}
struct App {
    auth: Auth,
    actions: Sender<Action>,
    events: Sender<Event>,
    input: String,
    input_cursor: usize,
    focus: Focus,
    results: Vec<SearchEntry>,
    result_limit: usize,
    query: String,
    has_more: bool,
    loading_more: bool,
    queue_generation: u64,
    context: Option<ContextMenu>,
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
    related: Vec<Track>,
    related_status: String,
    related_selected: usize,
    radio_loading: bool,
    lyrics_open: bool,
    lyrics_scroll: u16,
    auth_open: bool,
    auth_selection: usize,
    details_open: bool,
    details_scroll: u16,
}
impl App {
    fn new(auth: Auth, actions: Sender<Action>, events: Sender<Event>) -> Self {
        Self {
            auth,
            actions,
            events,
            input: String::new(),
            input_cursor: 0,
            focus: Focus::Search,
            results: vec![],
            result_limit: 50,
            query: String::new(),
            has_more: false,
            loading_more: false,
            queue_generation: 0,
            context: None,
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
            related: Vec::new(),
            related_status: String::new(),
            related_selected: 0,
            radio_loading: false,
            lyrics_open: false,
            lyrics_scroll: 0,
            auth_open: false,
            auth_selection: 0,
            details_open: false,
            details_scroll: 0,
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
        self.query = query;
        self.result_limit = 50;
        self.has_more = false;
        self.lookup(false);
    }
    fn lookup(&mut self, more: bool) {
        if more {
            self.result_limit = self.result_limit.saturating_add(50);
        }
        let query = self.query.clone();
        self.search_id = self.search_id.wrapping_add(1);
        let id = self.search_id;
        self.searching = true;
        self.loading_more = more;
        self.message = if more {
            "Загружаем ещё…"
        } else {
            "Ищем…"
        }
        .into();
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
                    self.related.clear();
                    self.related_selected = 0;
                    self.radio_loading = false;
                    self.related_status = if state.current.is_some() {
                        "Загружаем похожие песни…"
                    } else {
                        ""
                    }
                    .into();
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
                            let result = music::details(&track.url);
                            let _ = events.send(Event::Details(id, result));
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
                        let selected_entry = self
                            .loading_more
                            .then(|| self.results.get(self.selected).cloned())
                            .flatten();
                        let count = if self.query.starts_with("http://")
                            || self.query.starts_with("https://")
                        {
                            tracks.len()
                        } else {
                            tracks
                                .iter()
                                .filter(|entry| matches!(entry, SearchEntry::Track(_)))
                                .count()
                        };
                        self.has_more = count >= self.result_limit;
                        self.results = tracks;
                        self.selected = selected_entry
                            .and_then(|entry| self.results.iter().position(|item| *item == entry))
                            .unwrap_or(0);
                        self.message = if !warning.is_empty() {
                            warning
                        } else if self.results.is_empty() {
                            "Ничего не найдено".into()
                        } else {
                            format!("Найдено: {}", self.results.len())
                        };
                        if !self.results.is_empty() && !self.loading_more {
                            self.focus = Focus::Results;
                        }
                    }
                    Err(error) => {
                        if !self.loading_more {
                            self.results.clear();
                        }
                        self.has_more = false;
                        self.message = error;
                    }
                }
                self.loading_more = false;
            }
            Event::Search(_, _) => {}
            Event::QueueTail(id, initial, result) if id == self.queue_generation => match result {
                Ok(entries) => {
                    let tracks: Vec<_> = entries
                        .into_iter()
                        .filter_map(|entry| match entry {
                            SearchEntry::Track(track) => Some(track),
                            _ => None,
                        })
                        .collect();
                    if tracks.starts_with(&initial) {
                        let tail = tracks[initial.len()..].to_vec();
                        if !tail.is_empty() {
                            self.send(Action::Add(tail));
                        }
                    } else {
                        let known: std::collections::HashSet<_> =
                            initial.iter().map(|track| track.url.as_str()).collect();
                        let mut seen = known.clone();
                        let tail: Vec<_> = tracks
                            .iter()
                            .filter(|track| seen.insert(track.url.as_str()))
                            .cloned()
                            .collect();
                        if !tail.is_empty() {
                            self.send(Action::Add(tail));
                        }
                    }
                }
                Err(error) => {
                    self.message = format!("Не удалось загрузить остаток очереди: {error}")
                }
            },
            Event::QueueTail(_, _, _) => {}
            Event::Cover(id, result) if id == self.cover_id => match result {
                Ok(image) => {
                    self.cover = Some(image);
                    self.cover_status.clear();
                }
                Err(e) => self.cover_status = e,
            },
            Event::Cover(_, _) => {}
            Event::Spectrum(levels) => self.levels = levels,
            Event::Details(id, result) if id == self.cover_id => match result {
                Ok(details) => {
                    self.lyrics = details.lyrics;
                    self.related = details.related;
                    self.related_status = if !details.related_error.is_empty() {
                        details.related_error
                    } else if self.related.is_empty() {
                        "Похожие песни не найдены".into()
                    } else {
                        String::new()
                    };
                }
                Err(error) => {
                    self.lyrics = format!("Текст недоступен: {error}");
                    self.related_status = format!("Похожие песни недоступны: {error}");
                }
            },
            Event::Details(_, _) => {}
            Event::Radio(id, generation, result)
                if id == self.cover_id && generation == self.queue_generation =>
            {
                self.radio_loading = false;
                match result {
                    Ok(tracks) if !tracks.is_empty() => {
                        let count = tracks.len();
                        if let Some(track) = &self.state.current {
                            self.send(Action::ReplaceQueue(track.url.clone(), tracks));
                            self.message = format!("Радио включено: {count} треков в очереди");
                        } else {
                            self.message = "Трек сменился: радио не включено".into();
                        }
                    }
                    Ok(_) => self.message = "Радио для этой песни недоступно".into(),
                    Err(error) => self.message = format!("Не удалось включить радио: {error}"),
                }
            }
            Event::Radio(_, _, _) => {}
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
    fn play_from(&mut self, track: &Track) {
        if self.focus == Focus::Related {
            if let Some(index) = self
                .related
                .get(self.related_selected)
                .filter(|item| *item == track)
                .map(|_| self.related_selected)
                .or_else(|| self.related.iter().position(|item| item == track))
            {
                self.queue_generation = self.queue_generation.wrapping_add(1);
                self.radio_loading = false;
                self.send(Action::PlayList(self.related[index..].to_vec()));
                return;
            }
        }
        let Some(index) = self
            .results
            .get(self.selected)
            .filter(|entry| matches!(entry, SearchEntry::Track(item) if item == track))
            .map(|_| self.selected)
            .or_else(|| {
                self.results
                    .iter()
                    .position(|entry| matches!(entry, SearchEntry::Track(item) if item == track))
            })
        else {
            if let Some(index) = self.related.iter().position(|item| item == track) {
                self.queue_generation = self.queue_generation.wrapping_add(1);
                self.radio_loading = false;
                self.send(Action::PlayList(self.related[index..].to_vec()));
            }
            return;
        };
        let tracks = self.results[index..]
            .iter()
            .filter_map(|entry| match entry {
                SearchEntry::Track(track) => Some(track.clone()),
                _ => None,
            })
            .collect();
        self.queue_generation = self.queue_generation.wrapping_add(1);
        self.radio_loading = false;
        if self.has_more {
            let initial = self
                .results
                .iter()
                .filter_map(|entry| match entry {
                    SearchEntry::Track(track) => Some(track.clone()),
                    _ => None,
                })
                .collect();
            let query = self.query.clone();
            let auth = self.auth.clone();
            let events = self.events.clone();
            let id = self.queue_generation;
            thread::spawn(move || {
                let result = backend::lookup(&query, &auth, usize::MAX).map(|(entries, _)| entries);
                let _ = events.send(Event::QueueTail(id, initial, result));
            });
        }
        self.send(Action::PlayList(tracks));
    }
    fn start_radio(&mut self) {
        let Some(track) = self.state.current.clone() else {
            return;
        };
        if self.radio_loading {
            return;
        }
        self.queue_generation = self.queue_generation.wrapping_add(1);
        self.radio_loading = true;
        self.message = "Загружаем радио…".into();
        let id = self.cover_id;
        let generation = self.queue_generation;
        let events = self.events.clone();
        thread::spawn(move || {
            let _ = events.send(Event::Radio(id, generation, music::radio(&track.url)));
        });
    }
    fn context_action(&mut self) {
        if let Some(menu) = self.context.take() {
            match menu.choice {
                0 => self.play_from(&menu.track),
                1 => self.send(Action::PlayNext(menu.track)),
                _ => self.send(Action::Add(vec![menu.track])),
            }
        }
    }
    fn mouse(&mut self, mouse: MouseEvent, area: Rect) {
        if area.width < 45 || area.height < 18 {
            return;
        }
        let contains = |r: Rect| {
            mouse.column >= r.x
                && mouse.column < r.right()
                && mouse.row >= r.y
                && mouse.row < r.bottom()
        };
        let scroll = match mouse.kind {
            MouseEventKind::ScrollUp => -1,
            MouseEventKind::ScrollDown => 1,
            _ => 0,
        };
        if let Some(menu) = self.context.as_mut() {
            let rect = context_rect(area, menu.x, menu.y);
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && contains(rect)
                && mouse.row > rect.y
                && mouse.row < rect.bottom() - 1
            {
                menu.choice = usize::from(mouse.row - rect.y - 1).min(2);
                self.context_action();
            } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) && !contains(rect) {
                self.context = None;
            }
            return;
        }
        if self.details_open {
            if scroll < 0 {
                self.details_scroll = self.details_scroll.saturating_sub(3);
            }
            if scroll > 0 {
                self.details_scroll = self.details_scroll.saturating_add(3);
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && !contains(centered(area, 75, 50))
            {
                self.details_open = false;
            }
            return;
        }
        if self.lyrics_open {
            if scroll < 0 {
                self.lyrics_scroll = self.lyrics_scroll.saturating_sub(3);
            }
            if scroll > 0 {
                self.lyrics_scroll = self.lyrics_scroll.saturating_add(3);
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && !contains(centered(area, 75, 80))
            {
                self.lyrics_open = false;
            }
            return;
        }
        if self.auth_open {
            let popup = centered(area, 70, 70);
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && contains(popup)
                && mouse.row > popup.y
            {
                let index = usize::from(mouse.row - popup.y - 1);
                if index < 5 {
                    self.auth_selection = index;
                    self.choose_browser();
                }
            } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) && !contains(popup) {
                self.auth_open = false;
            }
            return;
        }
        if self.queue_open {
            let popup = centered(area, 75, 70);
            let inner = panel("", false).inner(popup);
            if scroll != 0 && contains(inner) {
                self.queue_selected = if scroll < 0 {
                    self.queue_selected.saturating_sub(1)
                } else {
                    (self.queue_selected + 1).min(self.state.queue.len().saturating_sub(1))
                };
            } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) && contains(inner) {
                let offset = list_offset(self.queue_selected, inner.height);
                let index = offset + usize::from(mouse.row - inner.y);
                if index < self.state.queue.len() {
                    self.queue_selected = index;
                }
            } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) && !contains(popup) {
                self.toggle_queue();
            }
            return;
        }
        let (left, right) = main_layout(area);
        let results = panel("", false).inner(left[1]);
        if scroll != 0 && contains(results) {
            self.focus = Focus::Results;
            self.selected = if scroll < 0 {
                self.selected.saturating_sub(3)
            } else {
                self.selected
                    .saturating_add(3)
                    .min(self.results.len().saturating_sub(1))
            };
            return;
        }
        let details = details_layout(right[0]);
        if scroll != 0 && contains(details.related) && !self.related.is_empty() {
            self.focus = Focus::Related;
            self.related_selected = if scroll < 0 {
                self.related_selected.saturating_sub(3)
            } else {
                self.related_selected
                    .saturating_add(3)
                    .min(self.related.len() - 1)
            };
            return;
        }
        if scroll != 0 && contains(details.lyrics) {
            self.lyrics_scroll = if scroll < 0 {
                self.lyrics_scroll.saturating_sub(3)
            } else {
                self.lyrics_scroll.saturating_add(3)
            };
            return;
        }
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }
        if contains(details.radio) && self.state.current.is_some() {
            self.start_radio();
        } else if contains(details.related) && !self.related.is_empty() {
            let index = list_offset(self.related_selected, details.related.height)
                + usize::from(mouse.row - details.related.y);
            if let Some(track) = self.related.get(index).cloned() {
                self.related_selected = index;
                self.focus = Focus::Related;
                self.context = Some(ContextMenu {
                    track,
                    choice: 0,
                    x: mouse.column,
                    y: mouse.row,
                });
            }
        } else if contains(left[0]) {
            self.focus = Focus::Search;
            let visible = usize::from(left[0].width.saturating_sub(2));
            let cursor_width = UnicodeWidthStr::width(&self.input[..self.input_cursor]);
            let scroll = cursor_width.saturating_sub(visible.saturating_sub(1));
            let column = usize::from(mouse.column.saturating_sub(left[0].x + 1)) + scroll;
            self.input_cursor = input_index_at_column(&self.input, column);
        } else if contains(results) {
            let artists = self
                .results
                .iter()
                .filter(|e| matches!(e, SearchEntry::Artist { .. }))
                .count();
            let albums = self
                .results
                .iter()
                .filter(|e| matches!(e, SearchEntry::Album { .. }))
                .count();
            let selected_row = result_row(self.selected, artists, albums, self.results.len());
            let row =
                list_offset(selected_row, results.height) + usize::from(mouse.row - results.y);
            if let Some(index) = (0..self.results.len())
                .find(|&i| result_row(i, artists, albums, self.results.len()) == row)
            {
                self.selected = index;
                self.focus = Focus::Results;
                match self.results[index].clone() {
                    SearchEntry::Track(track) => {
                        self.context = Some(ContextMenu {
                            track,
                            choice: 0,
                            x: mouse.column,
                            y: mouse.row,
                        })
                    }
                    SearchEntry::Artist { url, .. } | SearchEntry::Album { url, .. } => {
                        self.search_query(url)
                    }
                }
            }
        } else {
            let player = panel("", false).inner(left[2]);
            if mouse.row == left[2].y && contains(left[2]) {
                let x = usize::from(mouse.column.saturating_sub(left[2].x + 1));
                let options = [
                    " Плеер · ",
                    "Очередь",
                    " · ",
                    "G вход",
                    " · ",
                    "L текст",
                    " · ",
                    "E сообщение",
                ];
                let mut start = 0;
                for (index, label) in options.iter().enumerate() {
                    let end = start + UnicodeWidthStr::width(*label);
                    if x >= start && x < end {
                        match index {
                            1 => self.toggle_queue(),
                            3 => self.auth_open = true,
                            5 => self.lyrics_open = true,
                            7 => {
                                self.details_scroll = 0;
                                self.details_open = true;
                            }
                            _ => {}
                        }
                        break;
                    }
                    start = end;
                }
            } else if contains(player) {
                self.focus = Focus::Controls;
                if mouse.row == player.y + 2 && player.width > 0 {
                    self.button = (usize::from(mouse.column - player.x) * 3
                        / usize::from(player.width))
                    .min(2);
                    self.activate_button();
                } else if mouse.row == player.y + 3 {
                    let bar = progress_rect(
                        Rect::new(player.x, player.y + 3, player.width, 1),
                        &self.state,
                    );
                    if contains(bar) && bar.width > 1 {
                        if let Some(duration) =
                            self.state.duration.filter(|d| d.is_finite() && *d > 0.0)
                        {
                            let fraction =
                                f64::from(mouse.column - bar.x) / f64::from(bar.width - 1);
                            self.send(Action::Seek(
                                (duration * fraction).min((duration - 0.2).max(0.0)),
                            ));
                        }
                    }
                }
            }
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
            return true;
        }
        if let Some(menu) = self.context.as_mut() {
            match key.code {
                KeyCode::Esc => self.context = None,
                KeyCode::Up => menu.choice = menu.choice.saturating_sub(1),
                KeyCode::Down => menu.choice = (menu.choice + 1).min(2),
                KeyCode::Enter => self.context_action(),
                _ => {}
            }
            return true;
        }
        if self.details_open {
            match key.code {
                KeyCode::Up => self.details_scroll = self.details_scroll.saturating_sub(1),
                KeyCode::Down => self.details_scroll = self.details_scroll.saturating_add(1),
                _ => self.details_open = false,
            }
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
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            if self.queue_open {
                self.toggle_queue();
            }
            self.focus = Focus::Controls;
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
                    if let Some((index, _)) =
                        self.input[..self.input_cursor].char_indices().next_back()
                    {
                        self.input.drain(index..self.input_cursor);
                        self.input_cursor = index;
                    }
                }
                KeyCode::Delete => {
                    if let Some(ch) = self.input[self.input_cursor..].chars().next() {
                        self.input
                            .drain(self.input_cursor..self.input_cursor + ch.len_utf8());
                    }
                }
                KeyCode::Left => {
                    self.input_cursor = self.input[..self.input_cursor]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
                KeyCode::Right => {
                    if let Some(ch) = self.input[self.input_cursor..].chars().next() {
                        self.input_cursor += ch.len_utf8();
                    }
                }
                KeyCode::Home => self.input_cursor = 0,
                KeyCode::End => self.input_cursor = self.input.len(),
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.input.insert(self.input_cursor, c);
                    self.input_cursor += c.len_utf8();
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
            KeyCode::Char('q') => return false,
            KeyCode::Esc => self.focus = Focus::Search,
            KeyCode::Char('/') => self.focus = Focus::Search,
            KeyCode::Char('g') => self.auth_open = true,
            KeyCode::Char('r') => self.start_radio(),
            KeyCode::Char('l') => self.lyrics_open = true,
            KeyCode::Char('e') => {
                self.details_scroll = 0;
                self.details_open = true;
            }
            KeyCode::Char('s') => {
                self.queue_generation = self.queue_generation.wrapping_add(1);
                self.radio_loading = false;
                self.send(Action::Stop);
            }
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
                Focus::Related => self.related_selected = self.related_selected.saturating_sub(1),
                _ => {}
            },
            KeyCode::Down => match self.focus {
                Focus::Results if self.selected + 1 >= self.results.len() => {
                    if !self.has_more {
                        self.focus = Focus::Controls
                    }
                }
                Focus::Results => self.selected += 1,
                Focus::Related => {
                    self.related_selected =
                        (self.related_selected + 1).min(self.related.len().saturating_sub(1))
                }
                _ => {}
            },
            KeyCode::Left if self.focus == Focus::Controls => {
                self.button = self.button.saturating_sub(1)
            }
            KeyCode::Right if self.focus == Focus::Controls => {
                self.button = (self.button + 1).min(2)
            }
            KeyCode::Right if self.focus == Focus::Results && !self.related.is_empty() => {
                self.focus = Focus::Related
            }
            KeyCode::Left if self.focus == Focus::Related => self.focus = Focus::Results,
            KeyCode::Enter => match self.focus {
                Focus::Results => match self.results.get(self.selected).cloned() {
                    Some(SearchEntry::Track(track)) => self.play_from(&track),
                    Some(SearchEntry::Artist { url, .. } | SearchEntry::Album { url, .. }) => {
                        self.search_query(url)
                    }
                    None => {}
                },
                Focus::Controls => self.activate_button(),
                Focus::Related => {
                    if let Some(track) = self.related.get(self.related_selected).cloned() {
                        self.play_from(&track);
                    }
                }
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
fn list_offset(selected: usize, height: u16) -> usize {
    selected.saturating_sub(usize::from(height.saturating_sub(1)))
}
fn result_row(index: usize, artists: usize, albums: usize, total: usize) -> usize {
    index
        + usize::from(artists > 0)
        + usize::from(albums > 0 && index >= artists)
        + usize::from(total > artists + albums && index >= artists + albums)
}
fn input_index_at_column(input: &str, column: usize) -> usize {
    let mut offset = 0;
    for (index, ch) in input.char_indices() {
        let width = ch.width().unwrap_or(0);
        if width > 0 && column < offset + width {
            return index;
        }
        offset += width;
    }
    input.len()
}
fn truncate_label(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > width - 1 {
            break;
        }
        result.push(ch);
        used += w;
    }
    result.push('…');
    result
}
fn format_time(value: Option<f64>) -> String {
    match value {
        Some(value) if value.is_finite() && value >= 0.0 => {
            let seconds = value as u64;
            format!("{}:{:02}", seconds / 60, seconds % 60)
        }
        _ => "--:--".into(),
    }
}
fn progress_rect(row: Rect, state: &State) -> Rect {
    let elapsed = format_time(state.position);
    let duration = format_time(state.duration);
    let count = usize::from(row.width)
        .saturating_sub(elapsed.len() + duration.len() + 2)
        .min(30);
    let total = elapsed.len() + duration.len() + count + 2;
    let start =
        row.x + (usize::from(row.width).saturating_sub(total) / 2 + elapsed.len() + 1) as u16;
    Rect::new(start, row.y, count as u16, row.height)
}
struct DetailsLayout {
    image: Rect,
    title: Rect,
    artist: Rect,
    radio: Rect,
    related: Rect,
    lyrics: Rect,
}
fn details_layout(area: Rect) -> DetailsLayout {
    let inner = panel("", false).inner(area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);
    let left = Rect::new(
        cols[0].x,
        cols[0].y,
        cols[0].width.saturating_sub(1),
        cols[0].height,
    );
    let image_height = left.height.saturating_sub(5).min(11);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(image_height),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(left);
    DetailsLayout {
        image: rows[2],
        title: rows[0],
        artist: rows[1],
        radio: rows[3],
        related: rows[4],
        lyrics: cols[1],
    }
}
fn main_layout(area: Rect) -> (std::rc::Rc<[Rect]>, std::rc::Rc<[Rect]>) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(area);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(8),
        ])
        .split(columns[0]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(14)])
        .split(columns[1]);
    (left, right)
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
    let (left, right) = main_layout(area);
    let search = if app.input.is_empty() {
        "Введите название, исполнителя или ссылку…"
    } else {
        &app.input
    };
    let visible_width = left[0].width.saturating_sub(2) as usize;
    let cursor_width = UnicodeWidthStr::width(&app.input[..app.input_cursor]);
    let scroll = cursor_width.saturating_sub(visible_width.saturating_sub(1));
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
        && app.context.is_none()
    {
        frame.set_cursor_position((
            left[0].x + 1 + (cursor_width - scroll) as u16,
            left[0].y + 1,
        ));
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
    let albums = app
        .results
        .iter()
        .filter(|e| matches!(e, SearchEntry::Album { .. }))
        .count();
    let tracks = app.results.len() - artists - albums;
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
    if albums > 0 {
        results.push(
            ListItem::new(" АЛЬБОМЫ").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    }
    for entry in app.results.iter().skip(artists).take(albums) {
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
    for entry in app.results.iter().skip(artists + albums) {
        results.push(ListItem::new(entry.title()));
    }
    let mut rs = ListState::default();
    if !app.results.is_empty() {
        let row = result_row(app.selected, artists, albums, app.results.len());
        rs.select(Some(row));
        *rs.offset_mut() = list_offset(row, left[1].height.saturating_sub(2));
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
        " Плеер · Очередь · G вход · L текст · E сообщение ",
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
    let elapsed = format_time(app.state.position);
    let duration = format_time(app.state.duration);
    let count = usize::from(progress_rect(player_rows[3], &app.state).width);
    let ratio = match (app.state.position, app.state.duration) {
        (Some(position), Some(duration)) if duration > 0.0 => (position / duration).clamp(0.0, 1.0),
        _ => 0.0,
    };
    let filled = (ratio * count as f64).round() as usize;
    let mut segments = vec![Span::raw(format!("{elapsed} "))];
    for index in 0..count {
        let timestamp = app
            .state
            .duration
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| (index as f64 + 0.5) * d / count as f64);
        let loaded = timestamp.is_some_and(|time| {
            app.state
                .buffered
                .iter()
                .any(|&(start, end)| time >= start && time <= end)
        });
        let (symbol, color) = if index < filled {
            ("▰", Color::Cyan)
        } else if loaded {
            ("▰", Color::Gray)
        } else {
            ("▱", Color::DarkGray)
        };
        segments.push(Span::styled(symbol, Style::default().fg(color)));
    }
    segments.push(Span::raw(format!(" {duration}")));
    let progress = Line::from(segments);
    frame.render_widget(
        Paragraph::new(progress).alignment(ratatui::layout::Alignment::Center),
        player_rows[3],
    );
    frame.render_widget(
        Paragraph::new(app.message.replace(['\n', '\r'], " "))
            .style(Style::default().fg(Color::Gray)),
        player_rows[4],
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
            *selection.offset_mut() =
                list_offset(app.queue_selected, popup.height.saturating_sub(2));
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
                .scroll((app.details_scroll, 0))
                .block(panel(" Сообщение · ↑↓ прокрутка · Esc закрыть ", true)),
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
    if let Some(menu) = &app.context {
        let rect = context_rect(area, menu.x, menu.y);
        frame.render_widget(Clear, rect);
        let options = ["Играть", "Играть следующим", "Добавить в очередь"];
        let mut state = ListState::default();
        state.select(Some(menu.choice));
        frame.render_stateful_widget(
            List::new(
                options
                    .iter()
                    .map(|item| ListItem::new(*item))
                    .collect::<Vec<_>>(),
            )
            .block(panel("", true))
            .highlight_style(highlight)
            .highlight_symbol("▸ "),
            rect,
            &mut state,
        );
    }
}

fn context_rect(area: Rect, x: u16, y: u16) -> Rect {
    let width = area.width.min(24);
    let height = area.height.min(5);
    Rect::new(
        x.min(area.right().saturating_sub(width)).max(area.x),
        y.min(area.bottom().saturating_sub(height)).max(area.y),
        width,
        height,
    )
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
    let layout = details_layout(area);
    if let Some(image) = app
        .cover
        .as_ref()
        .filter(|_| has_color && layout.image.height > 0)
    {
        let side = layout
            .image
            .width
            .min(layout.image.height.saturating_mul(2))
            .min(22) as u32;
        if side > 0 {
            let image = image::imageops::thumbnail(image, side, side);
            let x = layout.image.x;
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
                    ratatui::layout::Rect::new(x, layout.image.y + row, image.width() as u16, 1),
                );
            }
        }
    } else if layout.image.height > 0 {
        let text = if !has_color && app.cover.is_some() {
            "Для обложки включите цвета терминала"
        } else {
            app.cover_status.as_str()
        };
        frame.render_widget(
            Paragraph::new(text),
            Rect::new(layout.image.x, layout.image.y, layout.image.width, 1),
        );
    }
    if let Some(track) = &app.state.current {
        let (artist, title) = track
            .title
            .split_once(" — ")
            .unwrap_or(("", track.title.as_str()));
        frame.render_widget(
            Paragraph::new(title).style(Style::default().add_modifier(Modifier::BOLD)),
            layout.title,
        );
        frame.render_widget(
            Paragraph::new(artist).style(Style::default().fg(Color::Gray)),
            layout.artist,
        );
        frame.render_widget(
            Paragraph::new(if app.radio_loading {
                "Загружаем радио…"
            } else {
                "▶ Радио по песне"
            })
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            layout.radio,
        );
        frame.render_widget(
            Paragraph::new(app.lyrics.as_str())
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((app.lyrics_scroll, 0))
                .style(Style::default().fg(Color::Gray)),
            layout.lyrics,
        );
        if app.related.is_empty() {
            frame.render_widget(
                Paragraph::new(app.related_status.as_str())
                    .style(Style::default().fg(Color::Gray))
                    .wrap(ratatui::widgets::Wrap { trim: true }),
                layout.related,
            );
        } else {
            let mut state = ListState::default();
            state.select(Some(app.related_selected));
            *state.offset_mut() = list_offset(app.related_selected, layout.related.height);
            frame.render_stateful_widget(
                List::new(
                    app.related
                        .iter()
                        .map(|track| {
                            ListItem::new(truncate_label(
                                &track.title,
                                usize::from(layout.related.width).saturating_sub(
                                    if app.focus == Focus::Related { 2 } else { 0 },
                                ),
                            ))
                        })
                        .collect::<Vec<_>>(),
                )
                .highlight_style(if app.focus == Focus::Related {
                    Style::default().bg(Color::DarkGray).fg(Color::White)
                } else {
                    Style::default()
                })
                .highlight_symbol(if app.focus == Focus::Related {
                    "▸ "
                } else {
                    ""
                }),
                layout.related,
                &mut state,
            );
        }
    }
}

fn draw_spectrum(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let block = panel("", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < spectrum::BANDS as u16 || inner.height == 0 {
        return;
    }
    let bands = spectrum::BANDS as u16;
    let bar_width = 1;
    let gap = (inner.width - bar_width * bands) / (bands - 1);
    let used = bar_width * bands + gap * (bands - 1);
    let start = inner.x + (inner.width - used) / 2;
    let height = inner.height.min(12);
    for index in 0..spectrum::BANDS {
        let lit = (app.levels[index].clamp(0.0, 1.0) * 24.0).round() as u16;
        for row in 0..height {
            let top = ((height - row) * 24 + height - 1) / height;
            let bottom = ((height - row - 1) * 24 + height - 1) / height + 1;
            if bottom > lit {
                continue;
            }
            let fraction = f32::from(top.min(lit)) / 24.0;
            let color = spectrum_color(fraction);
            let segment = if lit >= top { "═" } else { "─" };
            frame.render_widget(
                Paragraph::new(segment).style(Style::default().fg(color)),
                ratatui::layout::Rect::new(
                    start + index as u16 * (bar_width + gap),
                    inner.y + inner.height - height + row,
                    bar_width,
                    1,
                ),
            );
        }
    }
}

fn spectrum_color(fraction: f32) -> Color {
    if fraction <= 0.5 {
        Color::Reset
    } else if fraction <= 10.0 / 12.0 {
        Color::Rgb(216, 139, 72)
    } else {
        Color::Rgb(121, 44, 67)
    }
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
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
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
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
            if app.focus == Focus::Results
                && app.has_more
                && !app.searching
                && app.selected.saturating_add(10) >= app.results.len()
            {
                app.lookup(true);
            }
            terminal.draw(|frame| draw(frame, &app))?;
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    TermEvent::Key(key) => {
                        if key.kind != KeyEventKind::Release && !app.key(key) {
                            break;
                        }
                    }
                    TermEvent::Mouse(mouse) => {
                        let size = terminal.size()?;
                        app.mouse(mouse, Rect::new(0, 0, size.width, size.height));
                    }
                    _ => {}
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
            Action::PlayList(tracks) => assert_eq!(tracks[0].title, "two"),
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
        assert_eq!(spectrum::BANDS, 20);
        assert_eq!(spectrum_color(0.5), Color::Reset);
        assert_eq!(spectrum_color(0.65), Color::Rgb(216, 139, 72));
        assert_eq!(spectrum_color(0.9), Color::Rgb(121, 44, 67));
    }
    #[test]
    fn spectrum_has_equal_bars_and_twenty_four_strokes() {
        let (mut app, _) = make_app();
        app.levels = [1.0; spectrum::BANDS];
        let backend = ratatui::backend::TestBackend::new(55, 14);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_spectrum(frame, frame.area(), &app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for y in 1..13 {
            let row: String = (1..54)
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .collect();
            let bars: Vec<_> = row.split_whitespace().collect();
            assert_eq!(bars.len(), 20);
            assert!(bars.iter().all(|bar| *bar == "═"));
        }
    }
    #[test]
    fn spectrum_renders_single_half_segment_and_spreads_columns() {
        let (mut app, _) = make_app();
        app.levels[0] = 1.0 / 24.0;
        app.levels[1] = 2.0 / 24.0;
        let backend = ratatui::backend::TestBackend::new(82, 14);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_spectrum(frame, frame.area(), &app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row: String = (1..81)
            .map(|x| buffer.cell((x, 12)).unwrap().symbol())
            .collect();
        let single = row.find('─').unwrap();
        let double = row.find('═').unwrap();
        let gap = row[single + '─'.len_utf8()..double].chars().count();
        assert!(gap >= 3, "gap was {gap}");
        let row_above: String = (1..81)
            .map(|x| buffer.cell((x, 11)).unwrap().symbol())
            .collect();
        assert!(!row_above.contains('═') && !row_above.contains('─'));
    }
    #[test]
    fn ctrl_arrows_reach_controls_and_mouse_activates_pause() {
        let (mut app, rx) = make_app();
        app.key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert!(app.focus == Focus::Controls);
        app.focus = Focus::Search;
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert!(app.focus == Focus::Controls);
        let area = Rect::new(0, 0, 120, 40);
        let (left, _) = main_layout(area);
        let player = panel("", false).inner(left[2]);
        app.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: player.x + player.width / 2,
                row: player.y + 2,
                modifiers: KeyModifiers::NONE,
            },
            area,
        );
        assert!(matches!(rx.try_recv(), Ok(Action::Toggle)));
    }
    #[test]
    fn escape_and_control_c_do_not_quit() {
        let (mut app, _) = make_app();
        app.focus = Focus::Results;
        assert!(app.key(press(KeyCode::Esc)));
        assert!(app.focus == Focus::Search);
        assert!(app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        app.focus = Focus::Results;
        assert!(!app.key(press(KeyCode::Char('q'))));
    }
    #[test]
    fn search_input_supports_unicode_cursor_and_editing() {
        let (mut app, _) = make_app();
        for c in "кот猫".chars() {
            app.key(press(KeyCode::Char(c)));
        }
        app.key(press(KeyCode::Left));
        app.key(press(KeyCode::Backspace));
        app.key(press(KeyCode::Char('р')));
        assert_eq!(app.input, "кор猫");
        app.key(press(KeyCode::Delete));
        assert_eq!(app.input, "кор");
        app.key(press(KeyCode::Home));
        app.key(press(KeyCode::Char('!')));
        app.key(press(KeyCode::End));
        app.key(press(KeyCode::Char('?')));
        assert_eq!(app.input, "!кор?");
        assert_eq!(app.input_cursor, app.input.len());
        assert_eq!(input_index_at_column("a猫z", 2), 1);
        assert_eq!(input_index_at_column("a猫z", 3), 4);
    }
    #[test]
    fn album_headers_and_selection_have_consistent_rows() {
        assert_eq!(result_row(0, 1, 2, 4), 1);
        assert_eq!(result_row(1, 1, 2, 4), 3);
        assert_eq!(result_row(2, 1, 2, 4), 4);
        assert_eq!(result_row(3, 1, 2, 4), 6);
    }
    #[test]
    fn progress_colors_distinguish_played_cached_and_missing_ranges() {
        let (mut app, _) = make_app();
        app.state.position = Some(20.0);
        app.state.duration = Some(100.0);
        app.state.buffered = vec![(0.0, 60.0), (80.0, 90.0)];
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let (left, _) = main_layout(Rect::new(0, 0, 120, 40));
        let player = panel("", false).inner(left[2]);
        let bar = progress_rect(
            Rect::new(player.x, player.y + 3, player.width, 1),
            &app.state,
        );
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((bar.x + 2, bar.y)).unwrap().fg, Color::Cyan);
        assert_eq!(buffer.cell((bar.x + 10, bar.y)).unwrap().fg, Color::Gray);
        assert_eq!(
            buffer.cell((bar.x + 20, bar.y)).unwrap().fg,
            Color::DarkGray
        );
        assert_eq!(buffer.cell((bar.x + 25, bar.y)).unwrap().fg, Color::Gray);
    }
    #[test]
    fn related_titles_are_clipped_within_their_column() {
        assert_eq!(truncate_label("Длинное название猫", 7), "Длинно…");
        assert_eq!(truncate_label("猫猫", 3), "猫…");
        let layout = details_layout(Rect::new(60, 0, 60, 26));
        assert!(layout.title.y < layout.artist.y && layout.artist.y < layout.image.y);
        assert!(layout.related.right() < layout.lyrics.x);
    }
    #[test]
    fn mouse_selects_visible_result_in_scrolled_list() {
        let (mut app, rx) = make_app();
        app.results = (0..60)
            .map(|i| {
                SearchEntry::Track(Track {
                    title: format!("song {i}"),
                    url: format!("track{i}"),
                })
            })
            .collect();
        app.selected = 40;
        let area = Rect::new(0, 0, 120, 30);
        let (left, _) = main_layout(area);
        let inner = panel("", false).inner(left[1]);
        let row = result_row(app.selected, 0, 0, app.results.len());
        let offset = list_offset(row, inner.height);
        let clicked = offset + 2;
        let index = clicked - 1;
        app.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: inner.x,
                row: inner.y + 2,
                modifiers: KeyModifiers::NONE,
            },
            area,
        );
        assert!(rx.try_recv().is_err());
        assert!(matches!(&app.context, Some(menu) if menu.track.url == format!("track{index}")));
        app.key(press(KeyCode::Enter));
        assert!(
            matches!(rx.try_recv(), Ok(Action::PlayList(tracks)) if tracks[0].url == format!("track{index}") && tracks.len() == 60-index)
        );
    }
    #[test]
    fn menu_actions_keep_track_order_and_auto_queue_later_results() {
        let (mut app, rx) = make_app();
        let track = |id: &str| Track {
            title: id.into(),
            url: id.into(),
        };
        app.results = vec![
            SearchEntry::Track(track("one")),
            SearchEntry::Track(track("two")),
        ];
        app.query = "query".into();
        app.has_more = false;
        app.context = Some(ContextMenu {
            track: track("one"),
            choice: 0,
            x: 0,
            y: 0,
        });
        app.context_action();
        assert!(
            matches!(rx.try_recv(), Ok(Action::PlayList(tracks)) if tracks == vec![track("one"), track("two")])
        );
        app.handle_event(Event::QueueTail(
            app.queue_generation,
            vec![track("one"), track("two")],
            Ok(vec![
                SearchEntry::Track(track("one")),
                SearchEntry::Track(track("two")),
                SearchEntry::Track(track("three")),
            ]),
        ));
        assert!(matches!(rx.try_recv(), Ok(Action::Add(tracks)) if tracks == vec![track("three")]));
        app.queue_generation += 1;
        app.handle_event(Event::QueueTail(
            app.queue_generation - 1,
            vec![track("one"), track("two")],
            Ok(vec![
                SearchEntry::Track(track("one")),
                SearchEntry::Track(track("two")),
                SearchEntry::Track(track("stale")),
            ]),
        ));
        assert!(rx.try_recv().is_err());
        app.context = Some(ContextMenu {
            track: track("two"),
            choice: 1,
            x: 0,
            y: 0,
        });
        app.context_action();
        assert!(matches!(rx.try_recv(), Ok(Action::PlayNext(item)) if item == track("two")));
        app.context = Some(ContextMenu {
            track: track("one"),
            choice: 2,
            x: 0,
            y: 0,
        });
        app.context_action();
        assert!(matches!(rx.try_recv(), Ok(Action::Add(items)) if items == vec![track("one")]));
    }
    #[test]
    fn loading_more_preserves_selection_and_focus() {
        let (mut app, _) = make_app();
        app.query = "test".into();
        app.search_id = 5;
        app.loading_more = true;
        app.focus = Focus::Controls;
        let track = SearchEntry::Track(Track {
            title: "one".into(),
            url: "one".into(),
        });
        app.results = vec![track.clone()];
        app.handle_event(Event::Search(
            5,
            Ok((
                vec![
                    track.clone(),
                    SearchEntry::Track(Track {
                        title: "two".into(),
                        url: "two".into(),
                    }),
                ],
                String::new(),
            )),
        ));
        assert!(app.focus == Focus::Controls);
        assert_eq!(app.results[app.selected], track);
        assert!(!app.has_more);
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
    fn artwork_metadata_radio_and_lyrics_use_separate_columns() {
        let (mut app, _) = make_app();
        app.state.current = Some(Track {
            title: "The Band — The Song".into(),
            url: "url".into(),
        });
        app.lyrics = "Lyrics on right".into();
        app.related = vec![Track {
            title: "Related track".into(),
            url: "related".into(),
        }];
        let backend = ratatui::backend::TestBackend::new(140, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let lines: Vec<String> = (0..40)
            .map(|y| {
                (0..140)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect()
            })
            .collect();
        let positions = |needle: &str| {
            lines
                .iter()
                .enumerate()
                .find_map(|(y, line)| {
                    line.find(needle)
                        .map(|x| (UnicodeWidthStr::width(&line[..x]), y))
                })
                .unwrap()
        };
        let (title_x, title_y) = positions("The Song");
        let (artist_x, artist_y) = positions("The Band");
        let (radio_x, radio_y) = positions("Радио по песне");
        let (related_x, related_y) = positions("Related track");
        let (lyrics_x, _) = positions("Lyrics on right");
        assert!(
            title_x < lyrics_x && artist_x < lyrics_x && radio_x < lyrics_x && related_x < lyrics_x
        , "title={title_x} artist={artist_x} radio={radio_x} related={related_x} lyrics={lyrics_x}");
        assert!(title_y < artist_y && artist_y < radio_y && radio_y < related_y);
    }

    #[test]
    fn clicking_progress_sends_seek_and_radio_replaces_queue() {
        let (mut app, rx) = make_app();
        app.state.current = Some(Track {
            title: "Band — Song".into(),
            url: "https://music.youtube.com/watch?v=id".into(),
        });
        app.state.duration = Some(120.0);
        app.state.position = Some(20.0);
        let area = Rect::new(0, 0, 120, 40);
        let (left, _) = main_layout(area);
        let player = panel("", false).inner(left[2]);
        let bar = progress_rect(
            Rect::new(player.x, player.y + 3, player.width, 1),
            &app.state,
        );
        app.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: bar.x + bar.width - 1,
                row: bar.y,
                modifiers: KeyModifiers::NONE,
            },
            area,
        );
        assert!(
            matches!(rx.try_recv(), Ok(Action::Seek(position)) if (position - 119.8).abs() < 0.01)
        );
        app.cover_id = 5;
        app.queue_generation = 7;
        app.radio_loading = true;
        let tracks = vec![Track {
            title: "mix".into(),
            url: "url".into(),
        }];
        app.handle_event(Event::Radio(5, 7, Ok(tracks.clone())));
        assert!(
            matches!(rx.try_recv(), Ok(Action::ReplaceQueue(source, items)) if source.ends_with("v=id") && items == tracks)
        );
        assert!(!app.radio_loading);
        app.handle_event(Event::Radio(4, 7, Ok(tracks)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn reordered_queue_tail_appends_new_songs_without_duplicates() {
        let (mut app, rx) = make_app();
        let track = |id: &str| Track {
            title: id.into(),
            url: id.into(),
        };
        app.queue_generation = 8;
        app.handle_event(Event::QueueTail(
            8,
            vec![track("one"), track("two")],
            Ok(vec![
                SearchEntry::Track(track("two")),
                SearchEntry::Track(track("three")),
                SearchEntry::Track(track("one")),
                SearchEntry::Track(track("three")),
                SearchEntry::Track(track("four")),
            ]),
        ));
        assert!(
            matches!(rx.try_recv(), Ok(Action::Add(items)) if items == vec![track("three"), track("four")])
        );
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
