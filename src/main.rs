//! Local Library — interfaz gráfica de escritorio (egui/eframe).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use local_library::browser::FolderBrowser;
use local_library::catalog::{self, format_size, Catalog, GameKind};
use local_library::drives::{self, Drive};
use local_library::install::{self, InstallEvent, InstallHandle, InstallPlan, InstallReport};

const OK_COLOR: Color32 = Color32::from_rgb(122, 199, 143);
const WARN_COLOR: Color32 = Color32::from_rgb(235, 183, 100);
const ERROR_COLOR: Color32 = Color32::from_rgb(235, 122, 112);
const VITA_COLOR: Color32 = Color32::from_rgb(128, 176, 255);
const PSP_COLOR: Color32 = Color32::from_rgb(133, 205, 156);
const HOMEBREW_COLOR: Color32 = Color32::from_rgb(232, 186, 124);

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("local-library {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "Local Library — catálogo e instalador de juegos de PS Vita y PSP\n\n\
             Uso: local-library [--version]\n\n\
             Interfaz gráfica. La versión de terminal es local-library-tui."
        );
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 740.0])
            .with_min_inner_size([880.0, 560.0])
            .with_app_id("local-library")
            .with_title("Local Library — PS Vita / PSP"),
        ..Default::default()
    };
    eframe::run_native(
        "Local Library — PS Vita / PSP",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}

#[derive(Clone, Copy)]
enum BrowserTarget {
    Root,
    Dest,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InstallTarget {
    Drive,
    Library,
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

struct App {
    root_input: String,
    catalog: Option<Catalog>,
    scan_error: Option<String>,
    selected: HashSet<usize>,
    filter: String,
    dest_input: String,
    drives: Vec<Drive>,
    browser: Option<(BrowserTarget, FolderBrowser)>,
    install_handle: Option<InstallHandle>,
    progress: ProgressState,
    installed: HashSet<usize>,
    drive_catalog: Option<Catalog>,
    copy_back: HashSet<usize>,
    install_target: InstallTarget,
    log: Vec<String>,
}

impl App {
    fn new() -> Self {
        Self {
            root_input: String::new(),
            catalog: None,
            scan_error: None,
            selected: HashSet::new(),
            filter: String::new(),
            dest_input: String::new(),
            drives: drives::detect_drives(),
            browser: None,
            install_handle: None,
            progress: ProgressState::default(),
            installed: HashSet::new(),
            drive_catalog: None,
            copy_back: HashSet::new(),
            install_target: InstallTarget::Drive,
            log: Vec::new(),
        }
    }

    fn push_log(&mut self, message: impl Into<String>) {
        self.log.push(message.into());
        if self.log.len() > 300 {
            self.log.remove(0);
        }
    }

    fn scan(&mut self) {
        self.progress.report = None;
        self.progress.error = None;
        let path = PathBuf::from(self.root_input.trim());
        match catalog::scan(&path) {
            Ok(found) => {
                let count = found.games.len();
                let warnings = found.warnings.len();
                self.push_log(format!(
                    "Catálogo: {count} juego(s) en «{}».",
                    found.root.display()
                ));
                if warnings > 0 {
                    self.push_log(format!("{warnings} aviso(s) durante el escaneo."));
                }
                self.selected.clear();
                self.filter.clear();
                self.catalog = Some(found);
                self.scan_error = None;
                self.refresh_installed();
            }
            Err(error) => {
                self.push_log(format!("Error: {error}"));
                self.scan_error = Some(error);
                self.catalog = None;
                self.selected.clear();
                self.installed.clear();
            }
        }
    }

    fn refresh_drives(&mut self) {
        self.drives = drives::detect_drives();
        self.refresh_installed();
    }

    fn refresh_installed(&mut self) {
        self.installed.clear();
        self.drive_catalog = None;
        self.copy_back.clear();
        let dest_text = self.dest_input.trim();
        if dest_text.is_empty() {
            return;
        }
        let dest = PathBuf::from(dest_text);
        if !dest.is_dir() {
            return;
        }
        if let Some(catalog) = &self.catalog {
            self.installed = catalog::installed_games(catalog, &dest);
        }
        // Lee el catálogo de la unidad para saber qué juegos no están en la librería local.
        let Ok(found) = catalog::scan(&dest) else {
            return;
        };
        if let Some(local) = &self.catalog {
            self.copy_back = catalog::missing_games(local, &found).into_iter().collect();
        }
        self.drive_catalog = Some(found);
    }

    fn open_browser(&mut self, target: BrowserTarget) {
        let input = match target {
            BrowserTarget::Root => &self.root_input,
            BrowserTarget::Dest => &self.dest_input,
        };
        let trimmed = input.trim();
        let start = if !trimmed.is_empty() && Path::new(trimmed).is_dir() {
            PathBuf::from(trimmed)
        } else {
            drives::home_dir().unwrap_or_else(|| PathBuf::from("."))
        };
        self.browser = Some((target, FolderBrowser::new(start)));
    }

    fn destination_targets(&self, dest: &Path) -> Vec<(String, PathBuf)> {
        let Some(catalog) = &self.catalog else {
            return Vec::new();
        };
        install::destination_folders(catalog, &self.selected, dest)
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

    fn collisions(&self) -> usize {
        self.selected
            .iter()
            .filter(|index| self.installed.contains(*index))
            .count()
    }

    /// Selecciona los índices indicados, omitiendo los juegos ya instalados.
    fn select_all_visible(&mut self, indices: &[usize]) {
        for &index in indices {
            if self.installed.contains(&index) {
                self.selected.remove(&index);
            } else {
                self.selected.insert(index);
            }
        }
    }

    fn build_plan(&self) -> Result<(InstallPlan, PathBuf), String> {
        let dest_text = self.dest_input.trim();
        if dest_text.is_empty() {
            return Err("Elige primero la unidad de destino.".to_string());
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
                self.push_log(format!("Instalando en «{}»…", dest.display()));
                self.install_target = InstallTarget::Drive;
                self.progress = ProgressState {
                    total: plan.total_bytes,
                    ..Default::default()
                };
                self.install_handle = Some(install::spawn(plan, dest));
            }
            Err(message) => {
                self.push_log(format!("Error: {message}"));
                self.progress.error = Some(message);
            }
        }
    }

    /// Marca todos los juegos indicados o los desmarca si ya estaban todos marcados.
    fn toggle_copy_back_all(&mut self, indices: &[usize]) {
        if indices.iter().all(|index| self.copy_back.contains(index)) {
            self.copy_back.clear();
        } else {
            self.copy_back.extend(indices.iter().copied());
        }
    }

    fn build_copy_back_plan(&self) -> Result<(InstallPlan, PathBuf), String> {
        let drive = self
            .drive_catalog
            .as_ref()
            .ok_or_else(|| "No se pudo leer la unidad de destino.".to_string())?;
        let local = self
            .catalog
            .as_ref()
            .ok_or_else(|| "Primero escanea tu librería local.".to_string())?;
        if self.copy_back.is_empty() {
            return Err("Selecciona al menos un juego de la unidad.".to_string());
        }
        let plan = install::build_plan(drive, &self.copy_back, &local.root)?;
        Ok((plan, local.root.clone()))
    }

    fn start_copy_back(&mut self) {
        match self.build_copy_back_plan() {
            Ok((plan, dest)) => {
                if let Err(message) = install::check_space(&dest, plan.total_bytes) {
                    self.push_log(format!("Error: {message}"));
                    self.progress = ProgressState {
                        error: Some(message),
                        ..Default::default()
                    };
                    return;
                }
                self.push_log(format!(
                    "Copiando {} juego(s) a «{}»…",
                    self.copy_back.len(),
                    dest.display()
                ));
                self.install_target = InstallTarget::Library;
                self.progress = ProgressState {
                    total: plan.total_bytes,
                    ..Default::default()
                };
                self.install_handle = Some(install::spawn(plan, dest));
            }
            Err(message) => {
                self.push_log(format!("Error: {message}"));
                self.progress.error = Some(message);
            }
        }
    }

    fn poll_install(&mut self, ctx: &egui::Context) {
        let Some(handle) = self.install_handle.take() else {
            return;
        };
        let mut finished = false;
        loop {
            match handle.rx.try_recv() {
                Ok(InstallEvent::Started { items, total_bytes }) => {
                    self.progress.total = total_bytes;
                    self.push_log(format!("Copiando {items} elemento(s)…"));
                }
                Ok(InstallEvent::Item { label, bytes, dest }) => {
                    self.push_log(format!("{label} → {}", dest.display()));
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
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn apply_report(&mut self, report: InstallReport) {
        let target = self.install_target;
        if report.cancelled {
            self.push_log("Instalación cancelada.".to_string());
        } else if report.failed.is_empty() {
            self.push_log(format!(
                "Instalación completada: {} elemento(s), {}.",
                report.installed,
                format_size(report.bytes)
            ));
        } else {
            for failure in &report.failed {
                self.push_log(format!("Error: {failure}"));
            }
            self.push_log(format!(
                "Instalación terminada con {} error(es).",
                report.failed.len()
            ));
        }
        for warning in &report.warnings {
            self.push_log(format!("Aviso: {warning}"));
        }
        self.progress.report = Some(report);
        if target == InstallTarget::Library {
            // La librería local tiene juegos nuevos: se vuelve a leer sin perder el resultado.
            let report = self.progress.report.take();
            self.scan();
            self.progress.report = report;
        }
        self.refresh_drives();
    }

    fn source_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Local Library");
            ui.label(RichText::new("Catálogo de PS Vita y PSP").weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Escanear")
                    .on_hover_text("Vuelve a leer app/, addcont/ y pspemu/")
                    .clicked()
                {
                    self.scan();
                }
                if ui.button("Examinar…").clicked() {
                    self.open_browser(BrowserTarget::Root);
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Carpeta de origen:");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.root_input)
                        .hint_text("Ej.: /run/media/usuario/PSVITA   o   D:\\ux0")
                        .desired_width(f32::INFINITY),
                )
                .lost_focus()
                && ui.input(|input| input.key_pressed(egui::Key::Enter))
            {
                self.scan();
            }
        });
        if let Some(catalog) = &self.catalog {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Origen: {}", catalog.root.display()))
                        .small()
                        .weak(),
                );
                if !catalog.warnings.is_empty() {
                    ui.label(
                        RichText::new(format!("· {} aviso(s)", catalog.warnings.len()))
                            .small()
                            .color(WARN_COLOR),
                    )
                    .on_hover_text(catalog.warnings.join("\n"));
                }
            });
        }
        ui.add_space(4.0);
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.add_space(28.0);
        ui.vertical_centered(|ui| {
            ui.heading("Selecciona tu carpeta de juegos");
            ui.add_space(8.0);
            ui.label("Puede ser la raíz de ux0 (la copia de tu tarjeta/memoria) o cualquier carpeta que contenga:");
            ui.add_space(12.0);
            ui.label(
                RichText::new(
                    "app/        juegos de PS Vita  (app/<TITLE_ID>/)\n\
                     addcont/    DLC                 (se instalan junto a su juego)\n\
                     pspemu/     PSP                 (pspemu/ISO/*.iso, pspemu/PSP/GAME/*)",
                )
                .monospace(),
            );
            ui.add_space(16.0);
            if ui.button("Examinar carpeta…").clicked() {
                self.open_browser(BrowserTarget::Root);
            }
            if let Some(error) = self.scan_error.clone() {
                ui.add_space(12.0);
                ui.colored_label(ERROR_COLOR, error);
            }
            ui.add_space(18.0);
            ui.label(
                RichText::new(
                    "Los títulos se leen de sce_sys/param.sfo, del EBOOT.PBP o del propio ISO.\n\
                     Al instalar se replica la estructura app/, addcont/ y pspemu/ en la unidad.",
                )
                .small()
                .weak(),
            );
        });
    }

    fn catalog_panel(&mut self, ui: &mut egui::Ui) {
        if self.catalog.is_none() {
            self.welcome(ui);
            return;
        }

        struct RowData {
            index: usize,
            title: String,
            kind: GameKind,
            id: String,
            location: &'static str,
            size: u64,
            has_dlc: bool,
        }

        let (root_display, total_games) = {
            let catalog = self.catalog.as_ref().expect("catálogo presente");
            (catalog.root.display().to_string(), catalog.games.len())
        };
        let needle = self.filter.trim().to_lowercase();
        let rows: Vec<RowData> = {
            let catalog = self.catalog.as_ref().expect("catálogo presente");
            catalog
                .games
                .iter()
                .enumerate()
                .filter(|(_, game)| {
                    needle.is_empty()
                        || game.title.to_lowercase().contains(&needle)
                        || game.id.to_lowercase().contains(&needle)
                })
                .map(|(index, game)| RowData {
                    index,
                    title: game.title.clone(),
                    kind: game.kind,
                    id: game.id.clone(),
                    location: game.kind.location(),
                    size: game.total_size(),
                    has_dlc: game.dlc.is_some(),
                })
                .collect()
        };
        let (selected_count, selected_bytes) = self.selected_stats();
        let visible_indices: Vec<usize> = rows.iter().map(|row| row.index).collect();

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{total_games} juego(s)")).strong());
            ui.separator();
            ui.label(format!(
                "{selected_count} seleccionado(s) · {}",
                format_size(selected_bytes)
            ));
            if !self.installed.is_empty() {
                ui.label(
                    RichText::new(format!("· {} ya instalados", self.installed.len()))
                        .color(OK_COLOR),
                )
                .on_hover_text("Juegos detectados en la unidad de destino");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Ninguno").clicked() {
                    self.selected.clear();
                }
                if ui
                    .button("Todos")
                    .on_hover_text("Selecciona los visibles que aún no están instalados")
                    .clicked()
                {
                    let indices = visible_indices.clone();
                    self.select_all_visible(&indices);
                }
            });
        });
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Buscar por título o ID…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("Origen: {root_display}"))
                .small()
                .weak(),
        );
        ui.separator();

        if rows.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label("No hay juegos que coincidan con la búsqueda.");
            });
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in &rows {
                    ui.horizontal(|ui| {
                        let mut checked = self.selected.contains(&row.index);
                        if ui.checkbox(&mut checked, "").changed() {
                            if checked {
                                self.selected.insert(row.index);
                            } else {
                                self.selected.remove(&row.index);
                            }
                        }
                        ui.label(RichText::new(&row.title).strong());
                        ui.label(
                            RichText::new(&row.id)
                                .monospace()
                                .small()
                                .color(Color32::from_rgb(150, 160, 175)),
                        );
                        let color = match row.kind {
                            GameKind::Vita => VITA_COLOR,
                            GameKind::PspIso => PSP_COLOR,
                            GameKind::PspHomebrew => HOMEBREW_COLOR,
                        };
                        ui.label(RichText::new(row.kind.label()).small().color(color));
                        if self.installed.contains(&row.index) {
                            ui.label(
                                RichText::new("✓ INSTALADO")
                                    .small()
                                    .color(OK_COLOR),
                            )
                            .on_hover_text(
                                "Ya existe en la unidad de destino; «Todos» no lo selecciona, pero puedes marcarlo para reinstalarlo.",
                            );
                        }
                        if row.has_dlc {
                            ui.label(RichText::new("DLC").small().color(WARN_COLOR))
                                .on_hover_text("Incluye addcont/<TITLE_ID>");
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {                                ui.label(RichText::new(format_size(row.size)).monospace());
                                ui.label(
                                    RichText::new(row.location)
                                        .small()
                                        .weak(),
                                );
                        });
                    });
                    ui.separator();
                }
            });
    }

    /// Lista los juegos de la unidad que no están en la librería local y permite copiarlos de vuelta.
    fn drive_library_panel(&mut self, ui: &mut egui::Ui) {
        let dest_text = self.dest_input.trim().to_string();
        if dest_text.is_empty() || !Path::new(&dest_text).is_dir() {
            return;
        }
        ui.add_space(6.0);
        if self.drive_catalog.is_none() {
            ui.label(
                RichText::new(
                    "La unidad no contiene juegos reconocibles (app/, addcont/ o pspemu/).",
                )
                .small()
                .weak(),
            );
            return;
        }
        let Some(local) = &self.catalog else {
            ui.label(
                RichText::new("Escanea tu librería local para comparar con la unidad.")
                    .small()
                    .weak(),
            );
            return;
        };

        struct MissingRow {
            index: usize,
            title: String,
            kind: GameKind,
            id: String,
            size: u64,
        }

        let local_root = local.root.display().to_string();
        let rows: Vec<MissingRow> = {
            let drive = self
                .drive_catalog
                .as_ref()
                .expect("catálogo de la unidad presente");
            catalog::missing_games(local, drive)
                .into_iter()
                .filter_map(|index| {
                    drive.games.get(index).map(|game| MissingRow {
                        index,
                        title: game.title.clone(),
                        kind: game.kind,
                        id: game.id.clone(),
                        size: game.total_size(),
                    })
                })
                .collect()
        };

        let selected = rows
            .iter()
            .filter(|row| self.copy_back.contains(&row.index))
            .count();
        let selected_bytes: u64 = rows
            .iter()
            .filter(|row| self.copy_back.contains(&row.index))
            .map(|row| row.size)
            .sum();
        let indices: Vec<usize> = rows.iter().map(|row| row.index).collect();

        egui::CollapsingHeader::new(format!(
            "En la unidad, fuera de tu librería ({})",
            rows.len()
        ))
        .default_open(true)
        .show(ui, |ui| {
            if rows.is_empty() {
                ui.label(
                    RichText::new("Todo lo que hay en la unidad ya está en tu librería.")
                        .small()
                        .weak(),
                );
                return;
            }

            egui::ScrollArea::vertical()
                .id_salt("juegos-en-unidad")
                .max_height(150.0)
                .show(ui, |ui| {
                    for row in &rows {
                        ui.horizontal(|ui| {
                            let mut checked = self.copy_back.contains(&row.index);
                            if ui.checkbox(&mut checked, "").changed() {
                                if checked {
                                    self.copy_back.insert(row.index);
                                } else {
                                    self.copy_back.remove(&row.index);
                                }
                            }
                            ui.label(RichText::new(&row.title).small().strong());
                            ui.label(
                                RichText::new(&row.id)
                                    .monospace()
                                    .small()
                                    .color(Color32::from_rgb(150, 160, 175)),
                            );
                            ui.label(
                                RichText::new(row.kind.label())
                                    .small()
                                    .color(Color32::from_rgb(150, 160, 175)),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(RichText::new(format_size(row.size)).small());
                                },
                            );
                        });
                    }
                });

            ui.add_space(4.0);
            let all_selected = selected == rows.len();
            ui.horizontal(|ui| {
                let label = if all_selected {
                    "Deseleccionar todos"
                } else {
                    "Seleccionar todos"
                };
                if ui
                    .button(label)
                    .on_hover_text("Marca o desmarca todos los juegos de la unidad")
                    .clicked()
                {
                    self.toggle_copy_back_all(&indices);
                }
                ui.label(
                    RichText::new(format!("{selected} de {} marcados", rows.len()))
                        .small()
                        .weak(),
                );
            });

            let can_copy = selected > 0 && self.install_handle.is_none();
            let text = if selected == 0 {
                "Copiar a mi librería".to_string()
            } else {
                format!(
                    "Copiar {selected} a mi librería · {}",
                    format_size(selected_bytes)
                )
            };
            let button = egui::Button::new(RichText::new(text).strong())
                .min_size(egui::vec2(ui.available_width(), 30.0));
            if ui
                .add_enabled(can_copy, button)
                .on_hover_text(format!("Se copiarán a «{local_root}»"))
                .clicked()
            {
                self.start_copy_back();
            }
        });
    }

    fn install_panel(&mut self, ui: &mut egui::Ui) {
        let (selected_count, required) = self.selected_stats();
        ui.add_space(6.0);
        ui.heading("Instalar en unidad extraíble");
        ui.add_space(6.0);
        ui.label(
            RichText::new("Punto de montaje o letra de la unidad de destino:")
                .small()
                .weak(),
        );
        let dest_response = ui.add(
            egui::TextEdit::singleline(&mut self.dest_input)
                .hint_text("Ej.: /run/media/usuario/PSVITA")
                .desired_width(f32::INFINITY),
        );
        if dest_response.lost_focus()
            || (dest_response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)))
        {
            self.refresh_installed();
        }
        ui.horizontal(|ui| {
            if ui.button("Examinar…").clicked() {
                self.open_browser(BrowserTarget::Dest);
            }
            if ui
                .button("Revisar")
                .on_hover_text("Comprueba qué juegos ya están en la unidad")
                .clicked()
            {
                self.refresh_installed();
            }
            if ui.button("Actualizar").clicked() {
                self.refresh_drives();
            }
        });

        if self.drives.is_empty() {
            ui.label(
                RichText::new(
                    "No se detectaron unidades montadas. Conecta la unidad y pulsa «Actualizar», \
                     o elige su punto de montaje con «Examinar…».",
                )
                .small()
                .weak(),
            );
        } else {
            let drives = self.drives.clone();
            let current = self.dest_input.trim().to_string();
            egui::ComboBox::from_id_salt("selector-unidad")
                .selected_text("Seleccionar unidad detectada…")
                .width(ui.available_width() - 8.0)
                .show_ui(ui, |ui| {
                    for drive in &drives {
                        let selected = current == drive.mount.display().to_string();
                        let label = format!(
                            "{} · {}{}",
                            drive.label,
                            format_size(drive.free),
                            if drive.removable {
                                " · extraíble"
                            } else {
                                ""
                            }
                        );
                        let details = format!(
                            "{}\nDispositivo: {}\nCapacidad: {}",
                            drive.mount.display(),
                            drive.device,
                            format_size(drive.total)
                        );
                        if ui
                            .selectable_label(selected, label)
                            .on_hover_text(details)
                            .clicked()
                        {
                            self.dest_input = drive.mount.display().to_string();
                            self.refresh_installed();
                        }
                    }
                });
        }

        ui.add_space(6.0);
        let dest = PathBuf::from(self.dest_input.trim());
        let dest_is_dir = dest.is_dir();
        if dest_is_dir {
            if let Some(free) = install::available_space(&dest) {
                let enough = free >= required;
                ui.label(format!(
                    "Libre: {} · Necesario: {}",
                    format_size(free),
                    format_size(required)
                ));
                if !enough && selected_count > 0 {
                    ui.colored_label(
                        ERROR_COLOR,
                        "No hay espacio suficiente para la selección actual.",
                    );
                }
            }
            if !self.installed.is_empty() {
                ui.colored_label(
                    OK_COLOR,
                    format!(
                        "{} juego(s) ya instalados en esta unidad.",
                        self.installed.len()
                    ),
                );
            }
            let collisions = self.collisions();
            if collisions > 0 {
                ui.colored_label(
                    WARN_COLOR,
                    format!(
                        "{collisions} juego(s) de la selección ya existen y se sobrescribirán."
                    ),
                );
            }
            ui.label(
                RichText::new("Se copiará manteniendo app/, addcont/ y pspemu/ en la raíz.")
                    .small()
                    .weak(),
            );
        } else if !self.dest_input.trim().is_empty() {
            ui.colored_label(
                ERROR_COLOR,
                "La ruta de destino no existe o no es una carpeta.",
            );
        }

        self.drive_library_panel(ui);

        let targets = if dest_is_dir {
            self.destination_targets(&dest)
        } else {
            Vec::new()
        };
        if !targets.is_empty() {
            ui.add_space(4.0);
            egui::CollapsingHeader::new(format!("Carpetas de destino ({})", targets.len()))
                .default_open(true)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for (label, path) in &targets {
                                let relative = path.strip_prefix(&dest).unwrap_or(path);
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new(label).small().strong());
                                    ui.label(
                                        RichText::new(format!("→ {}", relative.display()))
                                            .small()
                                            .monospace()
                                            .color(Color32::from_rgb(150, 160, 175)),
                                    );
                                })
                                .response
                                .on_hover_text(path.display().to_string());
                            }
                        });
                });
        }

        ui.add_space(8.0);
        let can_install = self.catalog.is_some()
            && selected_count > 0
            && dest_is_dir
            && self.install_handle.is_none();
        let text = if selected_count == 0 {
            "Instalar juegos".to_string()
        } else {
            format!("Instalar {selected_count} juego(s)")
        };
        let button = egui::Button::new(RichText::new(text).strong())
            .min_size(egui::vec2(ui.available_width(), 34.0));
        if ui.add_enabled(can_install, button).clicked() {
            self.start_install();
        }

        if self.install_handle.is_some() {
            ui.add_space(8.0);
            let fraction = if self.progress.total > 0 {
                (self.progress.copied as f32 / self.progress.total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .show_percentage()
                    .text(format!(
                        "{} / {}",
                        format_size(self.progress.copied),
                        format_size(self.progress.total)
                    )),
            );
            ui.label(RichText::new(&self.progress.item).small());
            if let Some(dest) = &self.progress.dest {
                ui.label(
                    RichText::new(format!("→ {}", dest.display()))
                        .small()
                        .monospace()
                        .color(Color32::from_rgb(150, 160, 175)),
                )
                .on_hover_text(dest.display().to_string());
            }
            if ui.button("Cancelar").clicked() {
                if let Some(handle) = &self.install_handle {
                    handle.cancel();
                }
                self.push_log("Cancelación solicitada…".to_string());
            }
        }

        if let Some(error) = self.progress.error.clone() {
            ui.add_space(8.0);
            ui.colored_label(ERROR_COLOR, error);
        }

        if let Some(report) = &self.progress.report {
            ui.add_space(8.0);
            if report.cancelled {
                ui.colored_label(
                    WARN_COLOR,
                    format!(
                        "Instalación cancelada tras copiar {}.",
                        format_size(report.bytes)
                    ),
                );
            } else if report.failed.is_empty() {
                ui.colored_label(
                    OK_COLOR,
                    format!(
                        "Instalación completada: {} elemento(s), {}.",
                        report.installed,
                        format_size(report.bytes)
                    ),
                );
            } else {
                ui.colored_label(
                    ERROR_COLOR,
                    format!(
                        "{} elemento(s) copiados, {} error(es).",
                        report.installed,
                        report.failed.len()
                    ),
                );
                for failure in &report.failed {
                    ui.label(RichText::new(failure).small().color(ERROR_COLOR));
                }
            }
            if !report.warnings.is_empty() {
                ui.collapsing("Avisos", |ui| {
                    for warning in &report.warnings {
                        ui.label(RichText::new(warning).small());
                    }
                });
            }
        }

        ui.add_space(10.0);
        ui.separator();
        egui::CollapsingHeader::new(format!("Registro ({})", self.log.len()))
            .default_open(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.log {
                            ui.label(RichText::new(line).small());
                        }
                    });
            });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.install_handle.is_some() {
                ui.spinner();
                ui.label(format!(
                    "Copiando {} · {} de {}",
                    self.progress.item,
                    format_size(self.progress.copied),
                    format_size(self.progress.total)
                ));
            } else if let Some(line) = self.log.last() {
                ui.label(RichText::new(line).small().weak());
            } else {
                ui.label(
                    RichText::new("Selecciona la carpeta de origen para empezar.")
                        .small()
                        .weak(),
                );
            }
        });
    }

    fn render_browser(&mut self, ctx: &egui::Context) {
        let Some((target, mut browser)) = self.browser.take() else {
            return;
        };

        let mut choose: Option<PathBuf> = None;
        let mut cancel = false;
        let mut open = true;

        egui::Window::new("Elegir carpeta")
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .default_height(500.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Subir").clicked() {
                        browser.go_up();
                    }
                    if let Some(home) = drives::home_dir() {
                        if ui.button("Inicio").clicked() {
                            browser.navigate(home);
                        }
                    }
                    for (label, path) in [
                        ("/", PathBuf::from("/")),
                        ("/run/media", PathBuf::from("/run/media")),
                        ("/media", PathBuf::from("/media")),
                        ("/mnt", PathBuf::from("/mnt")),
                    ] {
                        if path.is_dir() && ui.button(label).clicked() {
                            browser.navigate(path);
                        }
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new(browser.current.display().to_string()).monospace());
                let mut show_hidden = browser.show_hidden;
                if ui
                    .checkbox(&mut show_hidden, "Mostrar carpetas ocultas")
                    .changed()
                {
                    browser.set_show_hidden(show_hidden);
                }
                ui.separator();

                if let Some(error) = &browser.error {
                    ui.colored_label(ERROR_COLOR, error);
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(330.0)
                    .show(ui, |ui| {
                        if browser.entries.is_empty() && browser.error.is_none() {
                            ui.label(RichText::new("No hay subcarpetas.").weak());
                        }
                        let entries = browser.entries.clone();
                        for entry in entries {
                            let label = format!("{}/", entry.name);
                            if ui.selectable_label(false, label).clicked() {
                                browser.navigate(entry.path);
                            }
                        }
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Seleccionar esta carpeta").clicked() {
                        choose = Some(browser.current.clone());
                    }
                    if ui.button("Cancelar").clicked() {
                        cancel = true;
                    }
                });
            });

        if let Some(path) = choose {
            match target {
                BrowserTarget::Root => {
                    self.root_input = path.display().to_string();
                    self.scan();
                }
                BrowserTarget::Dest => {
                    self.dest_input = path.display().to_string();
                    self.refresh_installed();
                }
            }
        } else if !cancel && open {
            self.browser = Some((target, browser));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_install(&ctx);

        egui::Panel::top("origen").show(ui, |ui| self.source_bar(ui));
        egui::Panel::bottom("estado").show(ui, |ui| self.status_bar(ui));
        egui::Panel::right("instalacion")
            .resizable(true)
            .default_size(370.0)
            .min_size(330.0)
            .show(ui, |ui| self.install_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| self.catalog_panel(ui));

        self.render_browser(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn toggle_selects_and_deselects_all_drive_games() {
        let mut app = App::new();
        app.copy_back.insert(2);

        app.toggle_copy_back_all(&[0, 1]);
        assert_eq!(app.copy_back.len(), 3);
        assert!(app.copy_back.contains(&0));
        assert!(app.copy_back.contains(&1));

        app.toggle_copy_back_all(&[0, 1]);
        assert!(app.copy_back.is_empty());
    }

    #[test]
    fn scans_and_builds_a_plan() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ux0");
        let dest = temp.path().join("usb");
        fs::create_dir_all(root.join("app").join("GAME1")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(root.join("app").join("GAME1").join("eboot.bin"), b"x").unwrap();

        fs::create_dir_all(dest.join("app").join("GAME1")).unwrap();
        fs::write(dest.join("app").join("GAME1").join("eboot.bin"), b"x").unwrap();

        let mut app = App::new();
        app.root_input = root.display().to_string();
        app.scan();

        let catalog = app.catalog.as_ref().expect("catálogo cargado");
        assert_eq!(catalog.games.len(), 1);
        app.selected.insert(0);
        app.dest_input = dest.display().to_string();
        app.refresh_installed();
        assert!(app.installed.contains(&0));

        let (plan, plan_dest) = app.build_plan().unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan_dest, dest);
        assert_eq!(plan.items[0].dest, dest.join("app/GAME1"));
    }

    #[test]
    fn select_all_ignores_installed_games() {
        let mut app = App::new();
        app.installed.insert(1);
        app.selected.insert(1);
        app.selected.insert(5);

        app.select_all_visible(&[0, 1, 2]);

        assert!(app.selected.contains(&0));
        assert!(app.selected.contains(&2));
        assert!(!app.selected.contains(&1));
        // Las selecciones fuera de la vista se conservan.
        assert!(app.selected.contains(&5));
    }

    #[test]
    fn lists_and_copies_games_missing_from_library() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        let drive = temp.path().join("usb");

        fs::create_dir_all(library.join("app").join("GAME1")).unwrap();
        fs::write(library.join("app").join("GAME1").join("eboot.bin"), b"x").unwrap();
        for name in ["GAME1", "GAME2"] {
            fs::create_dir_all(drive.join("app").join(name)).unwrap();
            fs::write(drive.join("app").join(name).join("eboot.bin"), b"x").unwrap();
        }

        let mut app = App::new();
        app.root_input = library.display().to_string();
        app.scan();
        app.dest_input = drive.display().to_string();
        app.refresh_installed();

        let drive_catalog = app.drive_catalog.as_ref().expect("unidad leída");
        let missing = drive_catalog
            .games
            .iter()
            .position(|game| game.id == "GAME2")
            .expect("GAME2 en la unidad");
        assert_eq!(app.copy_back.len(), 1);
        assert!(app.copy_back.contains(&missing));

        let (plan, plan_dest) = app.build_copy_back_plan().unwrap();
        assert_eq!(plan_dest, library);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].dest, library.join("app/GAME2"));

        let cancel = std::sync::atomic::AtomicBool::new(false);
        let report = install::run(&plan, &plan_dest, &cancel, &mut |_| {});
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(library.join("app/GAME2/eboot.bin").is_file());
    }

    #[test]
    fn scan_reports_invalid_folder() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new();
        app.root_input = temp.path().display().to_string();
        app.scan();
        assert!(app.catalog.is_none());
        assert!(app.scan_error.is_some());
    }
}
