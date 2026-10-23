use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Clear, Gauge, HighlightSpacing, List, ListItem, ListState, Paragraph, Row, Table,
    TableState, Wrap,
};
use ratatui::{DefaultTerminal, Frame};

use local_library::browser::FolderBrowser;
use local_library::catalog::{self, format_size, Catalog, GameKind};
use local_library::drives::{self, Drive};
use local_library::install::{self, InstallEvent, InstallHandle, InstallPlan, InstallReport};

const MIN_WIDTH: u16 = 76;
const MIN_HEIGHT: u16 = 18;

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Root,
    Dest,
}

impl Target {
    fn label(self) -> &'static str {
        match self {
            Target::Root => "origen",
            Target::Dest => "destino",
        }
    }
}

#[derive(Clone, Copy)]
enum StatusKind {
    Info,
    Success,
    Error,
}

struct Status {
    text: String,
    kind: StatusKind,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            text: "Elige la carpeta de origen con E (examinar) u O (escribir ruta).".to_string(),
            kind: StatusKind::Info,
        }
    }
}

#[derive(Default)]
struct ProgressState {
    total: u64,
    copied: u64,
    item: String,
    dest: Option<PathBuf>,
    report: Option<InstallReport>,
    error: Option<String>,
}

enum Modal {
    Browse {
        target: Target,
        browser: FolderBrowser,
        cursor: usize,
    },
    EditPath {
        target: Target,
        input: String,
    },
    Drives {
        cursor: usize,
    },
    Targets {
        cursor: usize,
    },
}

struct App {
    root_input: String,
    catalog: Option<Catalog>,
    filter: String,
    selected: HashSet<usize>,
    visible: Vec<usize>,
    cursor: usize,
    table_state: TableState,
    dest_input: String,
    drives: Vec<Drive>,
    modal: Option<Modal>,
    install_handle: Option<InstallHandle>,
    progress: ProgressState,
    installed: HashSet<usize>,
    status: Status,
    search_active: bool,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            root_input: String::new(),
            catalog: None,
            filter: String::new(),
            selected: HashSet::new(),
            visible: Vec::new(),
            cursor: 0,
            table_state: TableState::default(),
            dest_input: String::new(),
            drives: drives::detect_drives(),
            modal: None,
            install_handle: None,
            progress: ProgressState::default(),
            installed: HashSet::new(),
            status: Status::default(),
            search_active: false,
            should_quit: false,
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.draw(frame))?;
            self.poll_install();
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.handle_key(key);
                    }
                }
            }
        }
        Ok(())
    }

    fn set_status(&mut self, text: impl Into<String>, kind: StatusKind) {
        self.status = Status {
            text: text.into(),
            kind,
        };
    }

    fn scan(&mut self) {
        self.progress.report = None;
        self.progress.error = None;
        let path = PathBuf::from(self.root_input.trim());
        match catalog::scan(&path) {
            Ok(found) => {
                let games = found.games.len();
                let warnings = found.warnings.len();
                self.catalog = Some(found);
                self.selected.clear();
                self.filter.clear();
                self.search_active = false;
                self.rebuild_visible();
                self.refresh_installed();
                let text = if warnings > 0 {
                    format!("Catálogo cargado: {games} juego(s), {warnings} aviso(s).")
                } else {
                    format!("Catálogo cargado: {games} juego(s).")
                };
                self.set_status(text, StatusKind::Success);
            }
            Err(error) => {
                self.catalog = None;
                self.visible.clear();
                self.selected.clear();
                self.installed.clear();
                self.cursor = 0;
                self.table_state.select(None);
                self.set_status(error, StatusKind::Error);
            }
        }
    }

    fn rebuild_visible(&mut self) {
        let needle = self.filter.trim().to_lowercase();
        self.visible = match &self.catalog {
            Some(catalog) => catalog
                .games
                .iter()
                .enumerate()
                .filter(|(_, game)| {
                    needle.is_empty()
                        || game.title.to_lowercase().contains(&needle)
                        || game.id.to_lowercase().contains(&needle)
                })
                .map(|(index, _)| index)
                .collect(),
            None => Vec::new(),
        };
        self.cursor = 0;
        if self.visible.is_empty() {
            self.table_state.select(None);
        } else {
            self.table_state.select(Some(0));
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() - 1;
        let next = (self.cursor as isize + delta).clamp(0, last as isize) as usize;
        self.cursor = next;
        self.table_state.select(Some(next));
    }

    fn toggle_current(&mut self) {
        let Some(index) = self.visible.get(self.cursor).copied() else {
            return;
        };
        if !self.selected.insert(index) {
            self.selected.remove(&index);
        }
    }

    fn select_all_visible(&mut self) {
        let mut omitted = 0;
        for index in &self.visible {
            if self.installed.contains(index) {
                self.selected.remove(index);
                omitted += 1;
            } else {
                self.selected.insert(*index);
            }
        }
        if omitted > 0 {
            self.set_status(
                format!("Seleccionados los no instalados; {omitted} ya instalado(s) omitido(s)."),
                StatusKind::Info,
            );
        }
    }

    fn refresh_drives(&mut self) {
        self.drives = drives::detect_drives();
        self.refresh_installed();
    }

    fn refresh_installed(&mut self) {
        self.installed.clear();
        let dest_text = self.dest_input.trim();
        if dest_text.is_empty() {
            return;
        }
        let dest = PathBuf::from(dest_text);
        if !dest.is_dir() {
            return;
        }
        let Some(catalog) = &self.catalog else {
            return;
        };
        self.installed = catalog::installed_games(catalog, &dest);
    }

    fn selected_stats(&self) -> (usize, u64) {
        let Some(catalog) = &self.catalog else {
            return (0, 0);
        };
        let mut count = 0;
        let mut bytes = 0;
        for index in &self.selected {
            if let Some(game) = catalog.games.get(*index) {
                count += 1;
                bytes += game.total_size();
            }
        }
        (count, bytes)
    }

    fn collisions(&self, _dest: &Path) -> usize {
        self.selected
            .iter()
            .filter(|index| self.installed.contains(*index))
            .count()
    }
    fn build_plan(&self) -> Result<(InstallPlan, PathBuf), String> {
        let dest_text = self.dest_input.trim();
        if dest_text.is_empty() {
            return Err("Elige primero la unidad de destino (U).".to_string());
        }
        let dest = PathBuf::from(dest_text);
        let catalog = self
            .catalog
            .as_ref()
            .ok_or_else(|| "Primero escanea una carpeta de origen.".to_string())?;
        let plan = install::build_plan(catalog, &self.selected, &dest)?;
        Ok((plan, dest))
    }

    fn start_install(&mut self) {
        match self.build_plan() {
            Ok((plan, dest)) => {
                self.progress = ProgressState {
                    total: plan.total_bytes,
                    ..Default::default()
                };
                self.install_handle = Some(install::spawn(plan, dest.clone()));
                self.set_status(
                    format!("Instalando en «{}»…", dest.display()),
                    StatusKind::Info,
                );
            }
            Err(message) => {
                self.progress.error = Some(message.clone());
                self.set_status(message, StatusKind::Error);
            }
        }
    }

    fn poll_install(&mut self) {
        let Some(handle) = self.install_handle.take() else {
            return;
        };
        let mut finished = false;
        loop {
            match handle.rx.try_recv() {
                Ok(InstallEvent::Started { items, total_bytes }) => {
                    self.progress.total = total_bytes;
                    self.set_status(format!("Copiando {items} elemento(s)…"), StatusKind::Info);
                }
                Ok(InstallEvent::Item { label, bytes, dest }) => {
                    self.progress.item = format!("{label} ({})", format_size(bytes));
                    self.progress.dest = Some(dest);
                }
                Ok(InstallEvent::Progress { copied }) => {
                    self.progress.copied = copied;
                }
                Ok(InstallEvent::Finished(report)) => {
                    self.apply_report(report);
                    finished = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if !finished {
            self.install_handle = Some(handle);
        }
    }

    fn apply_report(&mut self, report: InstallReport) {
        let (text, kind) = if report.cancelled {
            (
                format!(
                    "Instalación cancelada tras copiar {}.",
                    format_size(report.bytes)
                ),
                StatusKind::Error,
            )
        } else if report.failed.is_empty() {
            (
                format!(
                    "Listo: {} elemento(s) copiados ({}).",
                    report.installed,
                    format_size(report.bytes)
                ),
                StatusKind::Success,
            )
        } else {
            (
                format!(
                    "Terminado con {} error(es); {} elemento(s) copiados.",
                    report.failed.len(),
                    report.installed
                ),
                StatusKind::Error,
            )
        };
        self.set_status(text, kind);
        self.progress.report = Some(report);
        self.refresh_drives();
    }

    fn request_quit(&mut self) {
        if self.install_handle.is_some() {
            self.set_status(
                "Hay una instalación en curso: pulsa C para cancelarla antes de salir.",
                StatusKind::Error,
            );
        } else {
            self.should_quit = true;
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            self.request_quit();
            return;
        }
        if self.modal.is_some() {
            self.handle_modal_key(key);
            return;
        }
        if self.search_active {
            self.handle_search_key(key);
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.request_quit(),
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.rebuild_visible();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::PageUp => self.move_cursor(-10),
            KeyCode::PageDown => self.move_cursor(10),
            KeyCode::Home => self.move_cursor(isize::MIN / 2),
            KeyCode::End => self.move_cursor(isize::MAX / 2),
            KeyCode::Char(' ') => self.toggle_current(),
            KeyCode::Char('a') | KeyCode::Char('A') => self.select_all_visible(),
            KeyCode::Char('n') | KeyCode::Char('N') => self.selected.clear(),
            KeyCode::Char('/') => {
                self.search_active = true;
                self.set_status(
                    "Escribe para filtrar · Enter aceptar · Esc limpiar",
                    StatusKind::Info,
                );
            }
            KeyCode::Char('e') | KeyCode::Char('E') => self.open_browser(Target::Root),
            KeyCode::Char('o') | KeyCode::Char('O') => self.open_edit_path(Target::Root),
            KeyCode::Char('b') | KeyCode::Char('B') => self.open_browser(Target::Dest),
            KeyCode::Char('t') | KeyCode::Char('T') => self.open_edit_path(Target::Dest),
            KeyCode::Char('v') | KeyCode::Char('V') => self.open_targets(),
            KeyCode::Char('u') | KeyCode::Char('U') => self.open_drives(),
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.refresh_drives();
                if self.root_input.trim().is_empty() {
                    self.set_status("Elige primero una carpeta de origen.", StatusKind::Error);
                } else {
                    self.scan();
                }
            }
            KeyCode::Enter => self.start_install(),
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if let Some(handle) = &self.install_handle {
                    handle.cancel();
                    self.set_status("Cancelación solicitada…", StatusKind::Info);
                }
            }
            _ => {}
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.search_active = false;
                self.rebuild_visible();
            }
            KeyCode::Enter => {
                self.search_active = false;
                self.set_status(
                    format!("{} juego(s) coinciden con el filtro.", self.visible.len()),
                    StatusKind::Info,
                );
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.rebuild_visible();
            }
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::PageUp => self.move_cursor(-10),
            KeyCode::PageDown => self.move_cursor(10),
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(character);
                self.rebuild_visible();
            }
            _ => {}
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) {
        let Some(mut modal) = self.modal.take() else {
            return;
        };
        let mut chosen: Option<(Target, PathBuf)> = None;
        let mut close = false;

        match &mut modal {
            Modal::Browse {
                target,
                browser,
                cursor,
            } => match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Up => *cursor = cursor.saturating_sub(1),
                KeyCode::Down => {
                    if *cursor + 1 < browser.entries.len() {
                        *cursor += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(entry) = browser.entries.get(*cursor) {
                        let path = entry.path.clone();
                        browser.navigate(path);
                        *cursor = 0;
                    }
                }
                KeyCode::Backspace => {
                    browser.go_up();
                    *cursor = 0;
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    chosen = Some((*target, browser.current.clone()));
                    close = true;
                }
                KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    browser.set_show_hidden(!browser.show_hidden);
                    *cursor = 0;
                }
                KeyCode::Char('h') | KeyCode::Char('H') => {
                    if let Some(home) = drives::home_dir() {
                        browser.navigate(home);
                        *cursor = 0;
                    }
                }
                KeyCode::Char('m') | KeyCode::Char('M') => {
                    for path in [
                        PathBuf::from("/run/media"),
                        PathBuf::from("/media"),
                        PathBuf::from("/mnt"),
                    ] {
                        if path.is_dir() {
                            browser.navigate(path);
                            *cursor = 0;
                            break;
                        }
                    }
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    browser.navigate(PathBuf::from("/"));
                    *cursor = 0;
                }
                _ => {}
            },
            Modal::EditPath { target, input } => match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Enter => {
                    let text = input.trim().to_string();
                    if !text.is_empty() {
                        chosen = Some((*target, PathBuf::from(text)));
                        close = true;
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.clear();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.push(character);
                }
                _ => {}
            },
            Modal::Drives { cursor } => match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Up => *cursor = cursor.saturating_sub(1),
                KeyCode::Down => {
                    if *cursor + 1 < self.drives.len() {
                        *cursor += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(drive) = self.drives.get(*cursor) {
                        self.dest_input = drive.mount.display().to_string();
                        self.refresh_installed();
                        close = true;
                    }
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.refresh_drives();
                    if *cursor >= self.drives.len() {
                        *cursor = self.drives.len().saturating_sub(1);
                    }
                }
                _ => {}
            },
            Modal::Targets { cursor } => {
                let count = self.destination_targets().len();
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => close = true,
                    KeyCode::Up => *cursor = cursor.saturating_sub(1),
                    KeyCode::Down => {
                        if count > 0 && *cursor + 1 < count {
                            *cursor += 1;
                        }
                    }
                    KeyCode::Home => *cursor = 0,
                    KeyCode::End => *cursor = count.saturating_sub(1),
                    _ => {}
                }
            }
        }

        if let Some((target, path)) = chosen {
            self.apply_path(target, path);
        }
        if !close {
            self.modal = Some(modal);
        }
    }

    fn apply_path(&mut self, target: Target, path: PathBuf) {
        match target {
            Target::Root => {
                self.root_input = path.display().to_string();
                self.scan();
            }
            Target::Dest => {
                self.dest_input = path.display().to_string();
                self.refresh_installed();
            }
        }
    }

    fn open_browser(&mut self, target: Target) {
        let input = match target {
            Target::Root => &self.root_input,
            Target::Dest => &self.dest_input,
        };
        let trimmed = input.trim();
        let start = if !trimmed.is_empty() && Path::new(trimmed).is_dir() {
            PathBuf::from(trimmed)
        } else {
            drives::home_dir().unwrap_or_else(|| PathBuf::from("."))
        };
        self.modal = Some(Modal::Browse {
            target,
            browser: FolderBrowser::new(start),
            cursor: 0,
        });
    }

    fn open_edit_path(&mut self, target: Target) {
        let input = match target {
            Target::Root => self.root_input.clone(),
            Target::Dest => self.dest_input.clone(),
        };
        self.modal = Some(Modal::EditPath { target, input });
    }

    fn destination_targets(&self) -> Vec<(String, PathBuf)> {
        let Some(catalog) = &self.catalog else {
            return Vec::new();
        };
        let dest = PathBuf::from(self.dest_input.trim());
        install::destination_folders(catalog, &self.selected, &dest)
    }

    fn open_targets(&mut self) {
        if self.selected.is_empty() {
            self.set_status(
                "Marca algún juego para ver su carpeta de destino.",
                StatusKind::Error,
            );
            return;
        }
        if self.dest_input.trim().is_empty() {
            self.set_status("Elige primero la unidad de destino (U).", StatusKind::Error);
            return;
        }
        self.modal = Some(Modal::Targets { cursor: 0 });
    }

    fn open_drives(&mut self) {
        self.refresh_drives();
        let cursor = self
            .drives
            .iter()
            .position(|drive| drive.mount.display().to_string() == self.dest_input.trim())
            .unwrap_or(0);
        self.modal = Some(Modal::Drives { cursor });
    }

    // ---------------------------------------------------------------- dibujo

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            frame.render_widget(
                Paragraph::new(format!(
                    "Terminal demasiado pequeña.\nSe necesitan al menos {MIN_WIDTH}x{MIN_HEIGHT} \
                     (actual: {}x{}).",
                    area.width, area.height
                ))
                .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }

        let sections = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(2),
            Constraint::Length(3),
        ])
        .split(area);

        self.draw_header(frame, sections[0]);
        self.draw_main(frame, sections[1]);
        self.draw_status(frame, sections[2]);
        self.draw_help(frame, sections[3]);
        self.draw_modal(frame, area);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Local Library — PS Vita / PSP ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let origin = if self.root_input.trim().is_empty() {
            "(sin seleccionar)".to_string()
        } else {
            self.root_input.trim().to_string()
        };
        let mut second = if self.search_active {
            vec![
                Span::styled("Buscar: ", Style::new().fg(Color::Cyan)),
                Span::raw(self.filter.clone()),
                Span::styled("█", Style::new().fg(Color::Cyan)),
                Span::styled(
                    format!("   {} resultado(s)", self.visible.len()),
                    Style::new().fg(Color::DarkGray),
                ),
            ]
        } else if let Some(catalog) = &self.catalog {
            let (selected, bytes) = self.selected_stats();
            let mut spans = vec![
                Span::styled(
                    format!("{} juego(s)", catalog.games.len()),
                    Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" · {selected} seleccionado(s) · {}", format_size(bytes)),
                    Style::new().fg(Color::Gray),
                ),
            ];
            if !self.filter.is_empty() {
                spans.push(Span::styled(
                    format!(" · filtro: «{}»", self.filter),
                    Style::new().fg(Color::Yellow),
                ));
            }
            if !self.installed.is_empty() {
                spans.push(Span::styled(
                    format!(" · {} ya instalados", self.installed.len()),
                    Style::new().fg(Color::LightGreen),
                ));
            }
            if !catalog.warnings.is_empty() {
                spans.push(Span::styled(
                    format!(" · {} aviso(s)", catalog.warnings.len()),
                    Style::new().fg(Color::Yellow),
                ));
            }
            spans
        } else {
            vec![Span::styled(
                "Sin catálogo cargado",
                Style::new().fg(Color::DarkGray),
            )]
        };

        if !self.search_active && self.filter.is_empty() && self.catalog.is_none() {
            second = vec![Span::styled(
                "La carpeta debe contener app/, addcont/ y pspemu/",
                Style::new().fg(Color::DarkGray),
            )];
        }

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("Origen: ", Style::new().fg(Color::DarkGray)),
                    Span::raw(origin),
                ]),
                Line::from(second),
            ]),
            inner,
        );
    }

    fn draw_main(&mut self, frame: &mut Frame, area: Rect) {
        let columns = Layout::horizontal([Constraint::Min(36), Constraint::Length(38)]).split(area);
        self.draw_catalog(frame, columns[0]);
        self.draw_install_panel(frame, columns[1]);
    }

    fn draw_catalog(&mut self, frame: &mut Frame, area: Rect) {
        let title = match &self.catalog {
            Some(catalog) => {
                let suffix = if self.search_active {
                    " · escribiendo búsqueda".to_string()
                } else if !self.filter.is_empty() {
                    format!(" · filtro: {}", self.filter)
                } else {
                    String::new()
                };
                format!(" Catálogo · {} juego(s){} ", catalog.games.len(), suffix)
            }
            None => " Catálogo ".to_string(),
        };
        let block = Block::bordered()
            .border_style(Style::new().fg(if self.search_active {
                Color::Yellow
            } else {
                Color::Cyan
            }))
            .title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.catalog.is_none() {
            let lines = vec![
                Line::from(Span::styled(
                    "Elige la carpeta que contiene tus juegos:",
                    Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  app/", Style::new().fg(Color::LightBlue)),
                    Span::raw("       juegos de PS Vita  (app/<TITLE_ID>/)"),
                ]),
                Line::from(vec![
                    Span::styled("  addcont/", Style::new().fg(Color::Yellow)),
                    Span::raw("   DLC  (se copian junto a su juego)"),
                ]),
                Line::from(vec![
                    Span::styled("  pspemu/", Style::new().fg(Color::Green)),
                    Span::raw("    PSP  (pspemu/ISO/*.iso, pspemu/PSP/GAME/*)"),
                ]),
                Line::from(""),
                Line::from(format!(
                    "Pulsa E para explorar o O para escribir la ruta.{}",
                    if self.root_input.trim().is_empty() {
                        ""
                    } else {
                        " Después, R para reescanear."
                    }
                )),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            return;
        }

        if self.visible.is_empty() {
            frame.render_widget(
                Paragraph::new("No hay juegos que coincidan con el filtro.")
                    .style(Style::new().fg(Color::DarkGray)),
                inner,
            );
            return;
        }

        let rows: Vec<Row> = self
            .visible
            .iter()
            .filter_map(|index| {
                let game = self.catalog.as_ref()?.games.get(*index)?;
                let marker = if self.selected.contains(index) {
                    "[x]"
                } else {
                    "[ ]"
                };
                let color = match game.kind {
                    GameKind::Vita => Color::LightBlue,
                    GameKind::PspIso => Color::LightGreen,
                    GameKind::PspHomebrew => Color::LightYellow,
                };
                Some(Row::new(vec![
                    Cell::from(Span::styled(
                        marker,
                        if self.selected.contains(index) {
                            Style::new().fg(Color::Green)
                        } else {
                            Style::new().fg(Color::DarkGray)
                        },
                    )),
                    Cell::from(Line::from({
                        let mut spans = vec![Span::raw(game.title.clone())];
                        if self.installed.contains(index) {
                            spans.push(Span::styled(" ✓", Style::new().fg(Color::LightGreen)));
                        }
                        spans.push(Span::styled(
                            format!("  [{}]", game.id),
                            Style::new().fg(Color::DarkGray),
                        ));
                        if game.dlc.is_some() {
                            spans.push(Span::styled(" +DLC", Style::new().fg(Color::Yellow)));
                        }
                        spans
                    })),
                    Cell::from(Span::styled(game.kind.label(), Style::new().fg(color))),
                    Cell::from(Span::styled(
                        format_size(game.total_size()),
                        Style::new().fg(Color::Gray),
                    )),
                ]))
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Fill(1),
                Constraint::Length(9),
                Constraint::Length(9),
            ],
        )
        .header(
            Row::new(vec!["", "Título", "Tipo", "Tamaño"]).style(
                Style::new()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .column_spacing(1)
        .highlight_spacing(HighlightSpacing::Always)
        .highlight_symbol("> ")
        .row_highlight_style(
            Style::new()
                .bg(Color::Rgb(24, 38, 52))
                .add_modifier(Modifier::BOLD),
        );
        frame.render_stateful_widget(table, inner, &mut self.table_state);
    }

    fn draw_install_panel(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Instalación ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (selected, required) = self.selected_stats();
        let dest_text = self.dest_input.trim().to_string();
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                "Destino",
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
            )),
            Line::from(if dest_text.is_empty() {
                "(sin elegir)".to_string()
            } else {
                dest_text.clone()
            }),
            Line::from(""),
        ];

        if !dest_text.is_empty() {
            let dest = Path::new(&dest_text);
            if dest.is_dir() {
                if let Some(free) = install::available_space(dest) {
                    let enough = free >= required;
                    lines.push(Line::from(Span::styled(
                        format!("Libre: {}", format_size(free)),
                        Style::new().fg(if enough { Color::Gray } else { Color::Red }),
                    )));
                }
                lines.push(Line::from(format!("Necesario: {}", format_size(required))));
                if !self.installed.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("{} ya instalados en la unidad", self.installed.len()),
                        Style::new().fg(Color::LightGreen),
                    )));
                }
                let collisions = self.collisions(dest);
                if collisions > 0 {
                    lines.push(Line::from(Span::styled(
                        format!("{collisions} ya existe(n): se sobrescribirá(n)"),
                        Style::new().fg(Color::Yellow),
                    )));
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "La ruta no existe o no es una carpeta.",
                    Style::new().fg(Color::Red),
                )));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "U unidad · B examinar · T ruta · V destinos",
            Style::new().fg(Color::DarkGray),
        )));

        let action_height = if self.install_handle.is_some() { 4 } else { 3 };
        let actions =
            Layout::vertical([Constraint::Min(1), Constraint::Length(action_height)]).split(inner);
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), actions[0]);

        let action_area = actions[1];
        let rows = Layout::vertical(vec![Constraint::Length(1); action_height as usize])
            .split(action_area);

        if self.install_handle.is_some() {
            let ratio = if self.progress.total > 0 {
                (self.progress.copied as f64 / self.progress.total as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let gauge = Gauge::default()
                .gauge_style(
                    Style::new()
                        .fg(Color::Cyan)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .ratio(ratio)
                .label(format!(
                    "{:.0}% · {} / {}",
                    ratio * 100.0,
                    format_size(self.progress.copied),
                    format_size(self.progress.total)
                ));
            frame.render_widget(gauge, rows[0]);
            frame.render_widget(
                Paragraph::new(self.progress.item.clone())
                    .style(Style::new().fg(Color::Gray))
                    .wrap(Wrap { trim: true }),
                rows[1],
            );
            let dest_line = self
                .progress
                .dest
                .as_ref()
                .map(|dest| format!("→ {}", dest.display()))
                .unwrap_or_default();
            frame.render_widget(
                Paragraph::new(dest_line)
                    .style(Style::new().fg(Color::DarkGray))
                    .wrap(Wrap { trim: true }),
                rows[2],
            );
            frame.render_widget(
                Paragraph::new("C: cancelar la instalación").style(Style::new().fg(Color::Yellow)),
                rows[3],
            );
        } else {
            let (hint, color) = self.install_hint(selected);
            frame.render_widget(
                Paragraph::new(hint).style(Style::new().fg(color).add_modifier(Modifier::BOLD)),
                rows[0],
            );
            let mut second = Line::from("");
            let mut third = Line::from("");
            if let Some(error) = &self.progress.error {
                second = Line::from(Span::styled(error.clone(), Style::new().fg(Color::Red)));
            } else if let Some(report) = &self.progress.report {
                if report.cancelled {
                    second = Line::from(Span::styled("Cancelada.", Style::new().fg(Color::Yellow)));
                } else if report.failed.is_empty() {
                    second = Line::from(Span::styled(
                        format!(
                            "Listo: {} elemento(s), {}.",
                            report.installed,
                            format_size(report.bytes)
                        ),
                        Style::new().fg(Color::Green),
                    ));
                } else {
                    second = Line::from(Span::styled(
                        format!(
                            "{} error(es), {} copiado(s).",
                            report.failed.len(),
                            report.installed
                        ),
                        Style::new().fg(Color::Red),
                    ));
                    if let Some(failure) = report.failed.first() {
                        third =
                            Line::from(Span::styled(failure.clone(), Style::new().fg(Color::Red)));
                    }
                }
            }
            frame.render_widget(Paragraph::new(second).wrap(Wrap { trim: true }), rows[1]);
            frame.render_widget(Paragraph::new(third).wrap(Wrap { trim: true }), rows[2]);
        }
    }

    fn install_hint(&self, selected: usize) -> (String, Color) {
        if self.catalog.is_none() {
            (
                "Escanea una carpeta de origen (E/O).".to_string(),
                Color::DarkGray,
            )
        } else if selected == 0 {
            ("Marca juegos con Espacio.".to_string(), Color::DarkGray)
        } else if self.dest_input.trim().is_empty() {
            (
                "Elige la unidad de destino (U).".to_string(),
                Color::DarkGray,
            )
        } else if !Path::new(self.dest_input.trim()).is_dir() {
            ("El destino no existe.".to_string(), Color::Red)
        } else if let Some(error) = &self.progress.error {
            (error.clone(), Color::Red)
        } else {
            (format!("Enter: instalar {selected} juego(s)"), Color::Green)
        }
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let (prefix, color) = match self.status.kind {
            StatusKind::Info => ("» ", Color::Gray),
            StatusKind::Success => ("✔ ", Color::Green),
            StatusKind::Error => ("✖ ", Color::Red),
        };
        let detail = self.highlighted_line();
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(prefix, Style::new().fg(color)),
                    Span::styled(self.status.text.clone(), Style::new().fg(color)),
                ]),
                detail,
            ])
            .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn highlighted_line(&self) -> Line<'static> {
        let Some(index) = self.visible.get(self.cursor).copied() else {
            return Line::from("");
        };
        let Some(game) = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.games.get(index))
        else {
            return Line::from("");
        };
        let color = match game.kind {
            GameKind::Vita => Color::LightBlue,
            GameKind::PspIso => Color::LightGreen,
            GameKind::PspHomebrew => Color::LightYellow,
        };
        let mut spans = vec![
            Span::styled(
                format!(" {}", game.kind.label()),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", game.title), Style::new().fg(Color::Gray)),
            Span::styled(
                format!("  ·  {}  ·  {}", game.id, game.kind.location()),
                Style::new().fg(Color::DarkGray),
            ),
        ];
        if game.dlc.is_some() {
            spans.push(Span::styled(
                format!("  ·  DLC {}", format_size(game.dlc_size)),
                Style::new().fg(Color::Yellow),
            ));
        }
        if self.installed.contains(&index) {
            spans.push(Span::styled(
                "  ·  YA INSTALADO",
                Style::new()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        Line::from(spans)
    }

    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let key = Style::new().fg(Color::White);
        let text = Style::new().fg(Color::DarkGray);
        let lines = vec![
            Line::from(vec![
                Span::styled("↑↓", key),
                Span::styled(" mover  ", text),
                Span::styled("Espacio", key),
                Span::styled(" marcar  ", text),
                Span::styled("A/N", key),
                Span::styled(" todos/ninguno  ", text),
                Span::styled("/", key),
                Span::styled(" buscar  ", text),
                Span::styled("R", key),
                Span::styled(" reescanear", text),
            ]),
            Line::from(vec![
                Span::styled("E", key),
                Span::styled(" examinar origen  ", text),
                Span::styled("O", key),
                Span::styled(" ruta origen  ", text),
                Span::styled("U", key),
                Span::styled(" unidades  ", text),
                Span::styled("B", key),
                Span::styled(" examinar destino  ", text),
                Span::styled("T", key),
                Span::styled(" ruta destino", text),
            ]),
            Line::from(vec![
                Span::styled("Enter", key),
                Span::styled(" instalar  ", text),
                Span::styled("V", key),
                Span::styled(" destinos  ", text),
                Span::styled("C", key),
                Span::styled(" cancelar  ", text),
                Span::styled("Q", key),
                Span::styled(" salir", text),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn draw_modal(&self, frame: &mut Frame, area: Rect) {
        match &self.modal {
            Some(Modal::Browse {
                target,
                browser,
                cursor,
            }) => {
                let popup = popup_rect(
                    area,
                    area.width.saturating_sub(10).min(100),
                    area.height.saturating_sub(6).min(32),
                );
                frame.render_widget(Clear, popup);
                let block = Block::bordered()
                    .border_style(Style::new().fg(Color::Yellow))
                    .title(format!(" Elegir carpeta de {} ", target.label()));
                let inner = block.inner(popup);
                frame.render_widget(block, popup);

                let sections = Layout::vertical([
                    Constraint::Length(2),
                    Constraint::Min(3),
                    Constraint::Length(2),
                ])
                .split(inner);
                frame.render_widget(
                    Paragraph::new(browser.current.display().to_string())
                        .style(Style::new().fg(Color::White))
                        .wrap(Wrap { trim: false }),
                    sections[0],
                );

                let items: Vec<ListItem> = browser
                    .entries
                    .iter()
                    .map(|entry| ListItem::new(format!(" {}/", entry.name)))
                    .collect();
                if items.is_empty() {
                    frame.render_widget(
                        Paragraph::new("No hay subcarpetas.")
                            .style(Style::new().fg(Color::DarkGray)),
                        sections[1],
                    );
                } else {
                    let mut state =
                        ListState::default().with_selected(Some((*cursor).min(items.len() - 1)));
                    let list = List::new(items).highlight_symbol("> ").highlight_style(
                        Style::new()
                            .bg(Color::Rgb(40, 40, 20))
                            .add_modifier(Modifier::BOLD),
                    );
                    frame.render_stateful_widget(list, sections[1], &mut state);
                }

                frame.render_widget(
                    Paragraph::new(
                        "Enter abrir · Backspace subir · S elegir esta · H inicio · M /run/media · R raíz · Ctrl+H ocultos · Esc cancelar",
                    )
                    .style(Style::new().fg(Color::DarkGray))
                    .wrap(Wrap { trim: true }),
                    sections[2],
                );
            }
            Some(Modal::EditPath { target, input }) => {
                let popup = popup_rect(area, 78, 9);
                frame.render_widget(Clear, popup);
                let block = Block::bordered()
                    .border_style(Style::new().fg(Color::Yellow))
                    .title(format!(" Ruta de {} ", target.label()));
                let inner = block.inner(popup);
                frame.render_widget(block, popup);
                let sections = Layout::vertical([
                    Constraint::Min(2),
                    Constraint::Length(1),
                    Constraint::Length(2),
                ])
                .split(inner);
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::raw(if input.is_empty() {
                            "Escribe la ruta…".to_string()
                        } else {
                            input.clone()
                        }),
                        Span::styled("█", Style::new().fg(Color::Yellow)),
                    ]))
                    .wrap(Wrap { trim: false }),
                    sections[0],
                );
                frame.render_widget(
                    Paragraph::new(if input.is_empty() {
                        String::new()
                    } else {
                        input.clone()
                    })
                    .style(Style::new().fg(Color::DarkGray))
                    .wrap(Wrap { trim: false }),
                    sections[1],
                );
                frame.render_widget(
                    Paragraph::new("Enter aceptar · Esc cancelar")
                        .style(Style::new().fg(Color::DarkGray)),
                    sections[2],
                );
            }
            Some(Modal::Drives { cursor }) => {
                let popup = popup_rect(
                    area,
                    area.width.saturating_sub(16).min(90),
                    area.height.saturating_sub(6).min(28),
                );
                frame.render_widget(Clear, popup);
                let block = Block::bordered()
                    .border_style(Style::new().fg(Color::Yellow))
                    .title(" Unidades detectadas ");
                let inner = block.inner(popup);
                frame.render_widget(block, popup);

                let sections =
                    Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(inner);
                if self.drives.is_empty() {
                    frame.render_widget(
                        Paragraph::new(
                            "No se detectaron unidades montadas.\nConecta la unidad y pulsa R, o usa B/T para escribir la ruta.",
                        )
                        .style(Style::new().fg(Color::DarkGray))
                        .wrap(Wrap { trim: true }),
                        sections[0],
                    );
                } else {
                    let items: Vec<ListItem> = self
                        .drives
                        .iter()
                        .map(|drive| {
                            ListItem::new(vec![
                                Line::from(vec![
                                    Span::styled(
                                        format!(" {} ", drive.label),
                                        Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(
                                        format!(
                                            "· {} libres{}",
                                            format_size(drive.free),
                                            if drive.removable {
                                                " · extraíble"
                                            } else {
                                                ""
                                            }
                                        ),
                                        Style::new().fg(Color::Gray),
                                    ),
                                ]),
                                Line::from(Span::styled(
                                    format!(
                                        "   {}  ·  {}  ·  {}",
                                        drive.mount.display(),
                                        drive.device,
                                        format_size(drive.total)
                                    ),
                                    Style::new().fg(Color::DarkGray),
                                )),
                            ])
                        })
                        .collect();
                    let mut state =
                        ListState::default().with_selected(Some((*cursor).min(items.len() - 1)));
                    let list = List::new(items).highlight_symbol("> ").highlight_style(
                        Style::new()
                            .bg(Color::Rgb(45, 40, 20))
                            .add_modifier(Modifier::BOLD),
                    );
                    frame.render_stateful_widget(list, sections[0], &mut state);
                }
                frame.render_widget(
                    Paragraph::new("Enter elegir · R actualizar · Esc cancelar")
                        .style(Style::new().fg(Color::DarkGray)),
                    sections[1],
                );
            }
            Some(Modal::Targets { cursor }) => {
                let popup = popup_rect(
                    area,
                    area.width.saturating_sub(10).min(110),
                    area.height.saturating_sub(6).min(32),
                );
                frame.render_widget(Clear, popup);
                let block = Block::bordered()
                    .border_style(Style::new().fg(Color::Yellow))
                    .title(" Carpetas de destino ");
                let inner = block.inner(popup);
                frame.render_widget(block, popup);

                let sections =
                    Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(inner);
                let targets = self.destination_targets();
                if targets.is_empty() {
                    frame.render_widget(
                        Paragraph::new("No hay juegos seleccionados.")
                            .style(Style::new().fg(Color::DarkGray)),
                        sections[0],
                    );
                } else {
                    let items: Vec<ListItem> = targets
                        .iter()
                        .map(|(label, path)| {
                            ListItem::new(vec![
                                Line::from(Span::styled(
                                    label.clone(),
                                    Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
                                )),
                                Line::from(Span::styled(
                                    format!("   {}", path.display()),
                                    Style::new().fg(Color::DarkGray),
                                )),
                            ])
                        })
                        .collect();
                    let mut state =
                        ListState::default().with_selected(Some((*cursor).min(items.len() - 1)));
                    let list = List::new(items).highlight_symbol("> ").highlight_style(
                        Style::new()
                            .bg(Color::Rgb(40, 40, 20))
                            .add_modifier(Modifier::BOLD),
                    );
                    frame.render_stateful_widget(list, sections[0], &mut state);
                }
                frame.render_widget(
                    Paragraph::new("Esc/Enter cerrar · ↑↓ mover")
                        .style(Style::new().fg(Color::DarkGray)),
                    sections[1],
                );
            }
            None => {}
        }
    }
}

fn popup_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width).max(1);
    let height = height.min(area.height).max(1);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::fs;

    fn app_with_games() -> (tempfile::TempDir, App) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ux0");
        for name in ["GAME1", "GAME2"] {
            fs::create_dir_all(root.join("app").join(name)).unwrap();
            fs::write(root.join("app").join(name).join("eboot.bin"), b"x").unwrap();
        }
        let catalog = catalog::scan(&root).unwrap();
        let mut app = App::new();
        app.root_input = root.display().to_string();
        app.catalog = Some(catalog);
        app.rebuild_visible();
        (temp, app)
    }

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "));
            }
            out.push('\n');
        }
        out
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn renders_catalog_and_panels() {
        let (temp, mut app) = app_with_games();
        app.dest_input = temp.path().display().to_string();
        app.selected.insert(app.visible[0]);

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text = screen(&terminal);

        assert!(text.contains("Local Library"));
        assert!(text.contains("Catálogo"));
        assert!(text.contains("GAME1"), "{text}");
        assert!(text.contains("GAME2"));
        assert!(text.contains("[x]"), "{text}");
        assert!(text.contains("Instalación"));
        assert!(text.contains("Destino"));
        assert!(text.contains("Enter: instalar 1 juego(s)"), "{text}");
        assert!(text.contains("Espacio"));
    }

    #[test]
    fn navigation_and_selection_update_state() {
        let (_temp, mut app) = app_with_games();
        assert_eq!(app.visible.len(), 2);

        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.cursor, 1);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.selected.contains(&app.visible[1]));
        app.handle_key(key(KeyCode::Char('a')));
        assert_eq!(app.selected.len(), 2);
        app.handle_key(key(KeyCode::Char('n')));
        assert!(app.selected.is_empty());
    }

    #[test]
    fn select_all_skips_installed_games() {
        let (_temp, mut app) = app_with_games();
        app.installed.insert(app.visible[0]);
        app.selected.insert(app.visible[0]);

        app.select_all_visible();

        assert!(!app.selected.contains(&app.visible[0]));
        assert!(app.selected.contains(&app.visible[1]));
        assert!(app.status.text.contains("omitido"));
    }

    #[test]
    fn search_filters_games() {
        let (_temp, mut app) = app_with_games();
        app.handle_key(key(KeyCode::Char('/')));
        assert!(app.search_active);
        for character in "GAME2".chars() {
            app.handle_key(key(KeyCode::Char(character)));
        }
        assert_eq!(app.visible.len(), 1);
        assert!(app
            .visible
            .iter()
            .any(|index| app.catalog.as_ref().unwrap().games[*index].id == "GAME2"));
        app.handle_key(key(KeyCode::Enter));
        assert!(!app.search_active);
    }

    #[test]
    fn renders_drives_modal() {
        let (_temp, mut app) = app_with_games();
        app.open_drives();
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Unidades detectadas"), "{text}");
        assert!(text.contains("Enter elegir"), "{text}");
    }

    #[test]
    fn renders_edit_path_modal() {
        let (_temp, mut app) = app_with_games();
        app.open_edit_path(Target::Dest);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Ruta de destino"), "{text}");
        assert!(text.contains("Enter aceptar"), "{text}");
    }

    #[test]
    fn install_hint_reports_missing_destination() {
        let (_temp, mut app) = app_with_games();
        app.selected.insert(app.visible[0]);
        let (hint, _) = app.install_hint(1);
        assert!(hint.contains("unidad de destino"));
        app.dest_input = "/ruta/que/no/existe".to_string();
        let (hint, _) = app.install_hint(1);
        assert!(hint.contains("no existe"));
    }
}
