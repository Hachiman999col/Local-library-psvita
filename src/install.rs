//! Copia de los juegos seleccionados a la raíz de la unidad extraíble,
//! replicando la estructura ux0 (app/, addcont/, pspemu/).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use crate::catalog::{Catalog, Game};

const CHUNK_SIZE: usize = 1024 * 1024;
const SPACE_MARGIN: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct InstallItem {
    pub label: String,
    pub source: PathBuf,
    pub dest: PathBuf,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct InstallPlan {
    pub items: Vec<InstallItem>,
    pub total_bytes: u64,
}

impl InstallPlan {
    pub fn for_games(games: &[&Game], dest_root: &Path) -> Self {
        let mut plan = Self::default();
        for game in games {
            plan.push(
                game.title.clone(),
                game.source.clone(),
                dest_root.join(&game.dest_rel),
                game.size,
            );
            if let Some(dlc) = &game.dlc {
                plan.push(
                    format!("{} (DLC)", game.title),
                    dlc.clone(),
                    dest_root.join("addcont").join(&game.id),
                    game.dlc_size,
                );
            }
        }
        plan
    }

    fn push(&mut self, label: String, source: PathBuf, dest: PathBuf, bytes: u64) {
        self.total_bytes += bytes;
        self.items.push(InstallItem {
            label,
            source,
            dest,
            bytes,
        });
    }
}

#[derive(Debug, Default)]
pub struct InstallReport {
    pub installed: usize,
    pub failed: Vec<String>,
    pub warnings: Vec<String>,
    pub cancelled: bool,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum InstallEvent {
    Started {
        items: usize,
        total_bytes: u64,
    },
    Item {
        label: String,
        bytes: u64,
        dest: PathBuf,
    },
    Progress {
        copied: u64,
    },
    Finished(InstallReport),
}

pub struct InstallHandle {
    pub rx: Receiver<InstallEvent>,
    cancel: Arc<AtomicBool>,
}

impl InstallHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Lanza la instalación en un hilo aparte para no bloquear la interfaz.
pub fn spawn(plan: InstallPlan, dest_root: PathBuf) -> InstallHandle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_thread = Arc::clone(&cancel);

    std::thread::spawn(move || {
        let mut emit = |event: InstallEvent| {
            let _ = tx.send(event);
        };
        let report = run(&plan, &dest_root, &cancel_thread, &mut emit);
        let _ = tx.send(InstallEvent::Finished(report));
    });

    InstallHandle { rx, cancel }
}

/// Ejecuta el plan de forma síncrona.
pub fn run(
    plan: &InstallPlan,
    _dest_root: &Path,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(InstallEvent),
) -> InstallReport {
    let mut report = InstallReport::default();
    emit(InstallEvent::Started {
        items: plan.items.len(),
        total_bytes: plan.total_bytes,
    });

    for item in &plan.items {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        emit(InstallEvent::Item {
            label: item.label.clone(),
            bytes: item.bytes,
            dest: item.dest.clone(),
        });

        let mut copied = 0u64;
        let already = report.bytes;
        let result = copy_path(
            &item.source,
            &item.dest,
            cancel,
            &mut report,
            &mut |bytes| {
                copied += bytes;
                emit(InstallEvent::Progress {
                    copied: already + copied,
                });
            },
        );
        report.bytes = already + copied;

        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        match result {
            Ok(()) => report.installed += 1,
            Err(message) => report.failed.push(format!("{}: {message}", item.label)),
        }
    }

    report
}

fn copy_path(
    source: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    report: &mut InstallReport,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(), String> {
    let meta = fs::symlink_metadata(source)
        .map_err(|error| format!("no se pudo leer «{}»: {error}", source.display()))?;

    if meta.file_type().is_symlink() {
        report.warnings.push(format!(
            "Se omitió el enlace simbólico «{}».",
            source.display()
        ));
        return Ok(());
    }

    if meta.is_dir() {
        fs::create_dir_all(dest)
            .map_err(|error| format!("no se pudo crear «{}»: {error}", dest.display()))?;
        let entries = fs::read_dir(source)
            .map_err(|error| format!("no se pudo leer «{}»: {error}", source.display()))?;
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelado".to_string());
            }
            let name = path
                .file_name()
                .map(PathBuf::from)
                .ok_or_else(|| format!("ruta inválida «{}»", path.display()))?;
            copy_path(&path, &dest.join(name), cancel, report, on_bytes)?;
        }
        Ok(())
    } else if meta.is_file() {
        copy_file(source, dest, cancel, on_bytes)
    } else {
        report.warnings.push(format!(
            "Se omitió «{}» (tipo de archivo no soportado).",
            source.display()
        ));
        Ok(())
    }
}

fn copy_file(
    source: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("no se pudo crear «{}»: {error}", parent.display()))?;
    }

    let partial = partial_path(dest);
    let result = (|| -> Result<(), String> {
        let mut input = File::open(source)
            .map_err(|error| format!("no se pudo abrir «{}»: {error}", source.display()))?;
        let expected = input
            .metadata()
            .map_err(|error| format!("no se pudo leer «{}»: {error}", source.display()))?
            .len();
        let mut output = File::create(&partial)
            .map_err(|error| format!("no se pudo crear «{}»: {error}", partial.display()))?;

        let mut buffer = vec![0u8; CHUNK_SIZE];
        let mut total = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelado".to_string());
            }
            let read = input
                .read(&mut buffer)
                .map_err(|error| format!("error leyendo «{}»: {error}", source.display()))?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|error| format!("error escribiendo «{}»: {error}", dest.display()))?;
            total += read as u64;
            on_bytes(read as u64);
        }
        output
            .flush()
            .map_err(|error| format!("error guardando «{}»: {error}", dest.display()))?;
        drop(output);

        if total != expected {
            return Err(format!(
                "tamaño inesperado en «{}» (se copiaron {total} de {expected} bytes)",
                source.display()
            ));
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            if dest.exists() {
                fs::remove_file(dest).map_err(|error| {
                    format!("no se pudo reemplazar «{}»: {error}", dest.display())
                })?;
            }
            fs::rename(&partial, dest)
                .map_err(|error| format!("no se pudo renombrar «{}»: {error}", dest.display()))?;
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&partial);
            Err(error)
        }
    }
}

fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// Carpetas de destino de la selección actual, en el orden del catálogo.
pub fn destination_folders(
    catalog: &Catalog,
    selected: &HashSet<usize>,
    dest_root: &Path,
) -> Vec<(String, PathBuf)> {
    let mut folders = Vec::new();
    for (index, game) in catalog.games.iter().enumerate() {
        if !selected.contains(&index) {
            continue;
        }
        folders.push((game.title.clone(), dest_root.join(&game.dest_rel)));
        if game.dlc.is_some() {
            folders.push((
                format!("{} (DLC)", game.title),
                dest_root.join("addcont").join(&game.id),
            ));
        }
    }
    folders
}

/// Construye y valida el plan de instalación para la selección y el destino dados.
pub fn build_plan(
    catalog: &Catalog,
    selected: &HashSet<usize>,
    dest: &Path,
) -> Result<InstallPlan, String> {
    if selected.is_empty() {
        return Err("Selecciona al menos un juego.".to_string());
    }
    if !dest.is_dir() {
        return Err(format!(
            "«{}» no es una carpeta o no está montada.",
            dest.display()
        ));
    }

    let dest_canonical =
        fs::canonicalize(dest).map_err(|error| format!("No se pudo abrir el destino: {error}"))?;
    let root_canonical = fs::canonicalize(&catalog.root).unwrap_or_else(|_| catalog.root.clone());
    if dest_canonical == root_canonical || dest_canonical.starts_with(&root_canonical) {
        return Err(
            "El destino no puede ser la carpeta de origen ni estar dentro de ella.".to_string(),
        );
    }

    let games: Vec<&Game> = selected
        .iter()
        .filter_map(|index| catalog.games.get(*index))
        .collect();
    let plan = InstallPlan::for_games(&games, dest);
    if plan.items.is_empty() {
        return Err("No hay nada que instalar.".to_string());
    }
    check_space(dest, plan.total_bytes)?;
    Ok(plan)
}

/// Espacio libre disponible en la unidad que contiene `path`.
pub fn available_space(path: &Path) -> Option<u64> {
    fs4::available_space(path).ok()
}

/// Comprueba que hay espacio suficiente (deja un margen de seguridad).
pub fn check_space(dest_root: &Path, required: u64) -> Result<u64, String> {
    let Some(free) = available_space(dest_root) else {
        return Err(format!(
            "No se pudo consultar el espacio libre de «{}».",
            dest_root.display()
        ));
    };
    if free < required.saturating_add(SPACE_MARGIN) {
        return Err(format!(
            "Espacio insuficiente: se necesitan {} y solo hay {} libres.",
            crate::catalog::format_size(required),
            crate::catalog::format_size(free)
        ));
    }
    Ok(free)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{format_size, Game, GameKind};

    fn fake_game(root: &Path, name: &str) -> Game {
        Game {
            kind: GameKind::Vita,
            title: name.to_string(),
            id: name.to_string(),
            source: root.join("app").join(name),
            dest_rel: PathBuf::from("app").join(name),
            size: 0,
            dlc: None,
            dlc_size: 0,
        }
    }

    #[test]
    fn installs_files_and_folders() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("origen");
        let dest = temp.path().join("usb");
        fs::create_dir_all(root.join("app").join("GAME1").join("sce_sys")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(
            root.join("app").join("GAME1").join("eboot.bin"),
            b"contenido",
        )
        .unwrap();
        fs::write(
            root.join("app")
                .join("GAME1")
                .join("sce_sys")
                .join("param.sfo"),
            b"sfo",
        )
        .unwrap();

        let game = fake_game(&root, "GAME1");
        let plan = InstallPlan::for_games(&[&game], &dest);
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();
        let report = run(&plan, &dest, &cancel, &mut |event| events.push(event));

        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(!report.cancelled);
        assert_eq!(report.installed, 1);
        assert_eq!(
            fs::read(dest.join("app").join("GAME1").join("eboot.bin")).unwrap(),
            b"contenido"
        );
        assert!(dest.join("app/GAME1/sce_sys/param.sfo").exists());
        assert!(events
            .iter()
            .any(|e| matches!(e, InstallEvent::Started { .. })));
    }

    #[test]
    fn includes_dlc_folder() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("origen");
        let dest = temp.path().join("usb");
        fs::create_dir_all(root.join("app").join("GAME1")).unwrap();
        fs::create_dir_all(root.join("addcont").join("GAME1")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(root.join("app").join("GAME1").join("eboot.bin"), b"juego").unwrap();
        fs::write(root.join("addcont").join("GAME1").join("dlc.bin"), b"dlc").unwrap();

        let mut game = fake_game(&root, "GAME1");
        game.dlc = Some(root.join("addcont").join("GAME1"));
        game.dlc_size = 3;

        let plan = InstallPlan::for_games(&[&game], &dest);
        let report = run(&plan, &dest, &AtomicBool::new(false), &mut |_| {});

        assert_eq!(report.installed, 2);
        assert_eq!(
            fs::read(dest.join("addcont").join("GAME1").join("dlc.bin")).unwrap(),
            b"dlc"
        );
    }

    #[test]
    fn cancellation_leaves_no_partial_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("origen");
        let dest = temp.path().join("usb");
        fs::create_dir_all(root.join("app").join("GAME1")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(
            root.join("app").join("GAME1").join("eboot.bin"),
            vec![7u8; 3 * 1024 * 1024],
        )
        .unwrap();

        let game = fake_game(&root, "GAME1");
        let plan = InstallPlan::for_games(&[&game], &dest);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_callback = Arc::clone(&cancel);
        let report = run(&plan, &dest, &cancel, &mut |event| {
            if matches!(event, InstallEvent::Progress { .. }) {
                cancel_for_callback.store(true, Ordering::Relaxed);
            }
        });

        assert!(report.cancelled);
        assert!(!dest.join("app/GAME1/eboot.bin").exists());
        assert!(!dest.join("app/GAME1/eboot.bin.part").exists());
    }

    #[test]
    fn installs_scanned_catalog_to_drive_root() {
        use crate::catalog::{scan, Game};

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ux0");
        std::fs::create_dir_all(root.join("app").join("GAME1")).unwrap();
        std::fs::create_dir_all(root.join("addcont").join("GAME1")).unwrap();
        std::fs::create_dir_all(root.join("pspemu").join("ISO")).unwrap();
        std::fs::create_dir_all(root.join("pspemu").join("PSP").join("GAME").join("HB")).unwrap();
        std::fs::write(root.join("app").join("GAME1").join("eboot.bin"), b"juego").unwrap();
        std::fs::write(root.join("addcont").join("GAME1").join("dlc.bin"), b"dlc").unwrap();
        std::fs::write(root.join("pspemu").join("ISO").join("Psp Game.iso"), b"iso").unwrap();
        std::fs::write(
            root.join("pspemu")
                .join("PSP")
                .join("GAME")
                .join("HB")
                .join("EBOOT.PBP"),
            b"pbp",
        )
        .unwrap();

        let catalog = scan(&root).unwrap();
        assert_eq!(catalog.games.len(), 3);

        let dest = temp.path().join("usb");
        std::fs::create_dir_all(&dest).unwrap();
        let games: Vec<&Game> = catalog.games.iter().collect();
        let plan = InstallPlan::for_games(&games, &dest);
        let report = run(&plan, &dest, &AtomicBool::new(false), &mut |_| {});

        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(dest.join("app/GAME1/eboot.bin").exists());
        assert!(dest.join("addcont/GAME1/dlc.bin").exists());
        assert!(dest.join("pspemu/ISO/Psp Game.iso").exists());
        assert!(dest.join("pspemu/PSP/GAME/HB/EBOOT.PBP").exists());
        assert_eq!(report.bytes, plan.total_bytes);
    }

    #[test]
    fn lists_destination_folders_for_selection() {
        use std::fs;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ux0");
        let dest = temp.path().join("usb");
        fs::create_dir_all(root.join("app").join("GAME1")).unwrap();
        fs::create_dir_all(root.join("addcont").join("GAME1")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(root.join("app").join("GAME1").join("eboot.bin"), b"x").unwrap();
        fs::write(root.join("addcont").join("GAME1").join("dlc.bin"), b"d").unwrap();

        let catalog = crate::catalog::scan(&root).unwrap();
        let mut selected = HashSet::new();
        selected.insert(0);
        let folders = destination_folders(&catalog, &selected, &dest);

        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].1, dest.join("app/GAME1"));
        assert!(folders[1].0.contains("DLC"));
        assert_eq!(folders[1].1, dest.join("addcont/GAME1"));
    }

    #[test]
    fn build_plan_validates_destination_and_selection() {
        use std::fs;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ux0");
        let dest = temp.path().join("usb");
        fs::create_dir_all(root.join("app").join("GAME1")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(root.join("app").join("GAME1").join("eboot.bin"), b"x").unwrap();

        let catalog = crate::catalog::scan(&root).unwrap();
        let mut selected = HashSet::new();
        assert!(build_plan(&catalog, &selected, &dest).is_err());

        selected.insert(0);
        let plan = build_plan(&catalog, &selected, &dest).unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].dest, dest.join("app/GAME1"));

        assert!(build_plan(&catalog, &selected, &root.join("app")).is_err());
        assert!(build_plan(&catalog, &selected, &temp.path().join("no-existe")).is_err());
    }

    #[test]
    fn reports_space_errors() {
        let temp = tempfile::tempdir().unwrap();
        let free = check_space(temp.path(), 1).unwrap();
        assert!(free > 0);
        assert!(format_size(free).ends_with('B'));
        assert!(check_space(temp.path(), u64::MAX / 2).is_err());
        assert!(check_space(&temp.path().join("no-existe"), 1).is_err());
    }
}
