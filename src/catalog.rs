//! Escaneo de una carpeta con estructura ux0 (app / addcont / pspemu) y
//! extracción de los datos de cada juego (título real, tamaño, DLC, destino).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const APP_DIR: &str = "app";
pub const ADDCONT_DIR: &str = "addcont";
pub const PSPEMU_DIR: &str = "pspemu";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GameKind {
    Vita,
    PspIso,
    PspHomebrew,
}

impl GameKind {
    pub fn label(self) -> &'static str {
        match self {
            GameKind::Vita => "PS Vita",
            GameKind::PspIso => "PSP ISO",
            GameKind::PspHomebrew => "PSP EBOOT",
        }
    }

    pub fn location(self) -> &'static str {
        match self {
            GameKind::Vita => "ux0:app",
            GameKind::PspIso => "ux0:pspemu/ISO",
            GameKind::PspHomebrew => "ux0:pspemu/PSP/GAME",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Game {
    pub kind: GameKind,
    /// Título legible (param.sfo o, si no existe, el nombre del archivo/carpeta).
    pub title: String,
    /// TITLE_ID de PS Vita / nombre de archivo o carpeta en PSP.
    pub id: String,
    pub source: PathBuf,
    /// Ruta relativa a la raíz de origen; se replica en la unidad de destino.
    pub dest_rel: PathBuf,
    pub size: u64,
    /// Carpeta `addcont/<TITLE_ID>` asociada (DLC), si existe.
    pub dlc: Option<PathBuf>,
    pub dlc_size: u64,
}

impl Game {
    pub fn total_size(&self) -> u64 {
        self.size + self.dlc_size
    }

    /// Comprueba si este juego ya está presente en la raíz de una unidad.
    pub fn is_installed_in(&self, dest_root: &Path) -> bool {
        let target = dest_root.join(&self.dest_rel);
        match self.kind {
            GameKind::Vita => {
                target.is_dir()
                    && fs::read_dir(&target)
                        .map(|mut entries| entries.next().is_some())
                        .unwrap_or(false)
            }
            GameKind::PspIso => target.is_file(),
            GameKind::PspHomebrew => target.join("EBOOT.PBP").is_file(),
        }
    }
}

/// Dos juegos representan el mismo contenido si coinciden tipo e identificador.
pub fn same_game(left: &Game, right: &Game) -> bool {
    left.kind == right.kind && left.id.eq_ignore_ascii_case(&right.id)
}

/// Juegos del catálogo externo que no existen en la librería local, por índice.
pub fn missing_games(local: &Catalog, external: &Catalog) -> Vec<usize> {
    external
        .games
        .iter()
        .enumerate()
        .filter(|(_, game)| !local.games.iter().any(|known| same_game(known, game)))
        .map(|(index, _)| index)
        .collect()
}

/// Índices del catálogo que ya existen en la unidad de destino.
pub fn installed_games(catalog: &Catalog, dest_root: &Path) -> HashSet<usize> {
    catalog
        .games
        .iter()
        .enumerate()
        .filter(|(_, game)| game.is_installed_in(dest_root))
        .map(|(index, _)| index)
        .collect()
}

#[derive(Clone, Debug)]
pub struct Catalog {
    pub root: PathBuf,
    pub games: Vec<Game>,
    pub warnings: Vec<String>,
}

/// Escanea `root` buscando `app/`, `addcont/` y `pspemu/`.
pub fn scan(root: &Path) -> Result<Catalog, String> {
    if !root.exists() {
        return Err(format!("La ruta «{}» no existe.", root.display()));
    }
    if !root.is_dir() {
        return Err(format!("«{}» no es una carpeta.", root.display()));
    }

    let has_any = [APP_DIR, ADDCONT_DIR, PSPEMU_DIR]
        .iter()
        .any(|d| root.join(d).is_dir());
    if !has_any {
        return Err(format!(
            "La carpeta «{}» no contiene app, addcont ni pspemu.",
            root.display()
        ));
    }

    let mut games = Vec::new();
    let mut warnings = Vec::new();

    scan_vita(root, &mut games, &mut warnings);
    scan_psp_isos(root, &mut games, &mut warnings);
    scan_psp_homebrew(root, &mut games, &mut warnings);

    games.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });

    Ok(Catalog {
        root: root.to_path_buf(),
        games,
        warnings,
    })
}

fn relative_to(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

fn scan_vita(root: &Path, games: &mut Vec<Game>, warnings: &mut Vec<String>) {
    let app_dir = root.join(APP_DIR);
    let dirs = read_dirs(&app_dir);
    if app_dir.is_dir() && dirs.is_empty() {
        warnings.push(format!("«{}» está vacía.", app_dir.display()));
    }

    let addcont_root = root.join(ADDCONT_DIR);
    let mut known_ids = Vec::new();

    for dir in dirs {
        let Some(id) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        known_ids.push(id.clone());

        let title = fs::read(dir.join("sce_sys").join("param.sfo"))
            .ok()
            .and_then(|data| sfo_string(&data, "TITLE"))
            .map(|t| clean_title(&t))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| id.clone());

        let dlc_dir = addcont_root.join(&id);
        let mut dlc = None;
        let mut dlc_size = 0;
        if dlc_dir.is_dir() {
            dlc_size = path_size(&dlc_dir);
            if dlc_size > 0 {
                dlc = Some(dlc_dir);
            }
        }

        games.push(Game {
            kind: GameKind::Vita,
            title,
            id,
            dest_rel: relative_to(root, &dir),
            source: dir.clone(),
            size: path_size(&dir),
            dlc,
            dlc_size,
        });
    }

    for dlc_dir in read_dirs(&addcont_root) {
        let Some(id) = dlc_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !known_ids.iter().any(|known| known == id) {
            warnings.push(format!(
                "DLC sin juego base en app/: addcont/{id} (no se incluye en el catálogo)."
            ));
        }
    }
}

fn scan_psp_isos(root: &Path, games: &mut Vec<Game>, warnings: &mut Vec<String>) {
    let mut iso_roots: Vec<PathBuf> = Vec::new();
    for candidate in [
        root.join(PSPEMU_DIR).join("ISO"),
        root.join(PSPEMU_DIR).join("iso"),
    ] {
        if !candidate.is_dir() {
            continue;
        }
        let canonical = fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        if iso_roots.iter().any(|existing| {
            fs::canonicalize(existing).unwrap_or_else(|_| existing.clone()) == canonical
        }) {
            continue;
        }
        iso_roots.push(candidate);
    }

    for iso_root in &iso_roots {
        let mut files = Vec::new();
        collect_isos(iso_root, 0, &mut files);
        if files.is_empty() {
            warnings.push(format!(
                "No se encontraron archivos .iso en «{}».",
                iso_root.display()
            ));
        }
        for file in files {
            let file_name = file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("desconocido")
                .to_string();
            let stem = Path::new(&file_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&file_name)
                .to_string();

            let title = iso_title(&file)
                .map(|t| clean_title(&t))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| stem.clone());

            let size = fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            games.push(Game {
                kind: GameKind::PspIso,
                title,
                id: file_name,
                source: file.clone(),
                dest_rel: relative_to(root, &file),
                size,
                dlc: None,
                dlc_size: 0,
            });
        }
    }
}

fn collect_isos(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() && depth < 2 {
            collect_isos(&path, depth + 1, out);
        } else if ft.is_file() {
            let is_iso = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("iso") || e.eq_ignore_ascii_case("cso"))
                .unwrap_or(false);
            if is_iso {
                out.push(path);
            }
        }
    }
}

fn scan_psp_homebrew(root: &Path, games: &mut Vec<Game>, warnings: &mut Vec<String>) {
    let game_root = root.join(PSPEMU_DIR).join("PSP").join("GAME");
    if !game_root.is_dir() {
        return;
    }

    let mut found = 0usize;
    for dir in read_dirs(&game_root) {
        let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        // Las carpetas de actualización de homebrew se llaman "%__SCE__...".
        if name.starts_with('%') {
            continue;
        }
        let eboot = dir.join("EBOOT.PBP");
        if !eboot.is_file() {
            continue;
        }
        let title = pbp_title(&eboot)
            .map(|t| clean_title(&t))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| name.clone());

        found += 1;
        games.push(Game {
            kind: GameKind::PspHomebrew,
            title,
            id: name,
            source: dir.clone(),
            dest_rel: relative_to(root, &dir),
            size: path_size(&dir),
            dlc: None,
            dlc_size: 0,
        });
    }

    if found == 0 {
        warnings.push(format!("«{}» no contiene EBOOT.PBP.", game_root.display()));
    }
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with('.'))
                .unwrap_or(false)
        })
        .collect();
    dirs.sort_by_cached_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_lowercase())
            .unwrap_or_default()
    });
    dirs
}

/// Tamaño total de un archivo o carpeta (sin seguir enlaces simbólicos).
pub fn path_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| path_size(&entry.path()))
        .sum()
}

fn clean_title(raw: &str) -> String {
    raw.trim()
        .trim_end_matches('\0')
        .replace('\0', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lee un valor de cadena de un archivo SFO (param.sfo).
pub fn sfo_string(data: &[u8], key: &str) -> Option<String> {
    if data.len() < 20 || &data[0..4] != b"\0PSF" {
        return None;
    }
    let read_u32 = |offset: usize| -> Option<u32> {
        Some(u32::from_le_bytes(
            data.get(offset..offset + 4)?.try_into().ok()?,
        ))
    };
    let key_table = read_u32(8)? as usize;
    let data_table = read_u32(12)? as usize;
    let count = read_u32(16)? as usize;
    if count > 1024 {
        return None;
    }

    for index in 0..count {
        let entry = 20 + index * 16;
        let key_offset = u16::from_le_bytes(data.get(entry..entry + 2)?.try_into().ok()?) as usize;
        let format = u16::from_le_bytes(data.get(entry + 2..entry + 4)?.try_into().ok()?);
        let length = read_u32(entry + 4)? as usize;
        let data_offset = read_u32(entry + 12)? as usize;

        let key_bytes = data.get(key_table.checked_add(key_offset)?..)?;
        let key_end = key_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(key_bytes.len());
        if &key_bytes[..key_end] != key.as_bytes() {
            continue;
        }
        // 0x0204 = cadena UTF-8; 0x0004 aparece en algunos SFO antiguos.
        if format != 0x0204 && format != 0x0004 {
            return None;
        }
        let start = data_table.checked_add(data_offset)?;
        let value = data.get(start..start.checked_add(length)?)?;
        return Some(
            String::from_utf8_lossy(value)
                .trim_end_matches('\0')
                .to_string(),
        );
    }
    None
}

fn read_pvd(file: &mut File) -> Option<[u8; 2048]> {
    let mut pvd = [0u8; 2048];
    file.seek(SeekFrom::Start(16 * 2048)).ok()?;
    file.read_exact(&mut pvd).ok()?;
    if &pvd[1..6] != b"CD001" {
        return None;
    }
    Some(pvd)
}

/// Lee el sistema de archivos ISO9660 de una imagen .iso y devuelve el TITLE
/// de PSP_GAME/PARAM.SFO. Para .cso (comprimido) no es posible sin descomprimir.
pub fn iso_title(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let pvd = read_pvd(&mut file)?;

    if let Some(title) = iso_fs_title(&mut file, &pvd) {
        return Some(title);
    }

    // Alternativa: identificador de volumen del ISO9660.
    let raw = &pvd[40..72];
    let label = String::from_utf8_lossy(raw)
        .trim_matches(|c: char| c == ' ' || c == '\0')
        .replace('_', " ");
    let label = clean_title(&label);
    if label.is_empty() || label.eq_ignore_ascii_case("cdrom") {
        None
    } else {
        Some(label)
    }
}

fn iso_fs_title(file: &mut File, pvd: &[u8; 2048]) -> Option<String> {
    let root = pvd.get(156..190)?;
    let root_lba = u32::from_le_bytes(root.get(2..6)?.try_into().ok()?);
    let root_len = u32::from_le_bytes(root.get(10..14)?.try_into().ok()?);
    let root_entries = read_iso_dir(file, root_lba, root_len)?;

    let psp_game = root_entries
        .iter()
        .find(|entry| entry.is_dir && entry.name.eq_ignore_ascii_case("PSP_GAME"))?;
    let game_entries = read_iso_dir(file, psp_game.extent, psp_game.size)?;
    let sfo = game_entries
        .iter()
        .find(|entry| !entry.is_dir && entry.name.eq_ignore_ascii_case("PARAM.SFO"))?;
    if sfo.size == 0 || sfo.size > 1024 * 1024 {
        return None;
    }

    let mut data = vec![0u8; sfo.size as usize];
    file.seek(SeekFrom::Start(sfo.extent as u64 * 2048)).ok()?;
    file.read_exact(&mut data).ok()?;
    sfo_string(&data, "TITLE")
}

#[derive(Debug)]
struct IsoEntry {
    name: String,
    extent: u32,
    size: u32,
    is_dir: bool,
}

fn read_iso_dir(file: &mut File, lba: u32, len: u32) -> Option<Vec<IsoEntry>> {
    const SECTOR: usize = 2048;
    if len == 0 || len as usize > 64 * 1024 * 1024 {
        return None;
    }
    let mut buffer = vec![0u8; len as usize];
    file.seek(SeekFrom::Start(lba as u64 * SECTOR as u64))
        .ok()?;
    file.read_exact(&mut buffer).ok()?;

    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset + 33 <= buffer.len() {
        let record_len = buffer[offset] as usize;
        if record_len == 0 {
            offset = (offset / SECTOR + 1) * SECTOR;
            continue;
        }
        if record_len < 33 || offset + record_len > buffer.len() {
            break;
        }
        let record = &buffer[offset..offset + record_len];
        let extent = u32::from_le_bytes(record.get(2..6)?.try_into().ok()?);
        let size = u32::from_le_bytes(record.get(10..14)?.try_into().ok()?);
        let flags = record.get(25).copied().unwrap_or(0);
        let name_len = record.get(32).copied().unwrap_or(0) as usize;
        if 33 + name_len <= record.len() {
            let raw = &record[33..33 + name_len];
            let is_dot = raw.len() == 1 && (raw[0] == 0 || raw[0] == 1);
            if !is_dot {
                let raw_name = String::from_utf8_lossy(raw);
                let name = raw_name
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('.')
                    .to_string();
                if !name.is_empty() {
                    entries.push(IsoEntry {
                        name,
                        extent,
                        size,
                        is_dir: flags & 0x02 != 0,
                    });
                }
            }
        }
        offset += record_len;
    }
    Some(entries)
}

/// Lee el TITLE del PARAM.SFO incluido en un EBOOT.PBP.
pub fn pbp_title(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; 0x28];
    file.read_exact(&mut header).ok()?;
    if &header[0..4] != b"\0PBP" {
        return None;
    }
    let sfo_start = u32::from_le_bytes(header.get(8..12)?.try_into().ok()?);
    let sfo_end = u32::from_le_bytes(header.get(12..16)?.try_into().ok()?);
    if sfo_end <= sfo_start || sfo_end - sfo_start > 1024 * 1024 {
        return None;
    }
    let mut data = vec![0u8; (sfo_end - sfo_start) as usize];
    file.seek(SeekFrom::Start(sfo_start as u64)).ok()?;
    file.read_exact(&mut data).ok()?;
    sfo_string(&data, "TITLE")
}

/// Formatea bytes en unidades legibles (base 1024).
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn build_sfo(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut keys = Vec::new();
        let mut values = Vec::new();
        let mut key_offsets = Vec::new();
        let mut value_offsets = Vec::new();
        for (key, value) in entries {
            key_offsets.push(keys.len() as u16);
            keys.extend_from_slice(key.as_bytes());
            keys.push(0);
            value_offsets.push(values.len() as u32);
            values.extend_from_slice(value.as_bytes());
            values.push(0);
        }

        let key_table_start = 20 + entries.len() * 16;
        let data_table_start = key_table_start + keys.len();
        let mut out = Vec::new();
        out.extend_from_slice(b"\0PSF");
        out.extend_from_slice(&0x0000_0101u32.to_le_bytes());
        out.extend_from_slice(&(key_table_start as u32).to_le_bytes());
        out.extend_from_slice(&(data_table_start as u32).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (index, ((_, value), _)) in entries.iter().zip(0..).enumerate() {
            out.extend_from_slice(&key_offsets[index].to_le_bytes());
            out.extend_from_slice(&0x0204u16.to_le_bytes());
            out.extend_from_slice(&((value.len() + 1) as u32).to_le_bytes());
            out.extend_from_slice(&((value.len() + 1) as u32).to_le_bytes());
            out.extend_from_slice(&value_offsets[index].to_le_bytes());
        }
        out.extend_from_slice(&keys);
        out.extend_from_slice(&values);
        out
    }

    fn write_dir_record(
        buf: &mut [u8],
        offset: usize,
        name: &str,
        extent: u32,
        size: u32,
        is_dir: bool,
    ) {
        let mut name_bytes = name.as_bytes().to_vec();
        if is_dir {
            name_bytes.push(b'.');
        } else {
            name_bytes.extend_from_slice(b";1");
        }
        let len = 33 + name_bytes.len();
        let pad = if len.is_multiple_of(2) { 0 } else { 1 };
        let total = len + pad;
        buf[offset] = total as u8;
        buf[offset + 2..offset + 6].copy_from_slice(&extent.to_le_bytes());
        buf[offset + 10..offset + 14].copy_from_slice(&size.to_le_bytes());
        buf[offset + 25] = if is_dir { 2 } else { 0 };
        buf[offset + 32] = name_bytes.len() as u8;
        buf[offset + 33..offset + 33 + name_bytes.len()].copy_from_slice(&name_bytes);
    }

    /// Crea un ISO9660 mínimo con PSP_GAME/PARAM.SFO.
    fn build_iso(path: &Path, title: &str) {
        let sfo = build_sfo(&[("TITLE", title), ("DISC_ID", "ULUS00001")]);
        let mut image = vec![0u8; 24 * 2048];

        {
            let pvd = &mut image[16 * 2048..17 * 2048];
            pvd[0] = 1;
            pvd[1..6].copy_from_slice(b"CD001");
            pvd[6] = 1;
            pvd[40..40 + 11].copy_from_slice(b"TEST_VOLUME");
            write_dir_record(pvd, 156, "", 18, 2048, true);
        }
        {
            let root = &mut image[18 * 2048..19 * 2048];
            let mut offset = 0;
            write_dir_record(root, offset, "", 18, 2048, true);
            offset += root[offset] as usize;
            write_dir_record(root, offset, "", 18, 2048, true);
            offset += root[offset] as usize;
            write_dir_record(root, offset, "PSP_GAME", 19, 2048, true);
        }
        {
            let dir = &mut image[19 * 2048..20 * 2048];
            let mut offset = 0;
            write_dir_record(dir, offset, "", 19, 2048, true);
            offset += dir[offset] as usize;
            write_dir_record(dir, offset, "", 19, 2048, true);
            offset += dir[offset] as usize;
            write_dir_record(dir, offset, "PARAM.SFO", 20, sfo.len() as u32, false);
        }
        image[20 * 2048..20 * 2048 + sfo.len()].copy_from_slice(&sfo);

        let mut file = File::create(path).unwrap();
        file.write_all(&image).unwrap();
    }

    #[test]
    fn reads_sfo_strings() {
        let sfo = build_sfo(&[("TITLE", "Un Juego Ñ"), ("TITLE_ID", "PCSX00001")]);
        assert_eq!(sfo_string(&sfo, "TITLE").as_deref(), Some("Un Juego Ñ"));
        assert_eq!(sfo_string(&sfo, "TITLE_ID").as_deref(), Some("PCSX00001"));
        assert_eq!(sfo_string(&sfo, "NOPE"), None);
        assert_eq!(sfo_string(b"basura", "TITLE"), None);
    }

    #[test]
    fn reads_title_from_iso_filesystem() {
        let temp = tempfile::tempdir().unwrap();
        let iso = temp.path().join("juego.iso");
        build_iso(&iso, "God of War: Chains of Olympus");
        assert_eq!(
            iso_title(&iso).as_deref(),
            Some("God of War: Chains of Olympus")
        );
    }

    #[test]
    fn reads_title_from_pbp() {
        let temp = tempfile::tempdir().unwrap();
        let pbp = temp.path().join("EBOOT.PBP");
        let sfo = build_sfo(&[("TITLE", "Homebrew PSP")]);
        let mut data = Vec::new();
        data.extend_from_slice(b"\0PBP");
        data.extend_from_slice(&0x0001_0000u32.to_le_bytes());
        data.extend_from_slice(&0x28u32.to_le_bytes()); // inicio param.sfo
        data.extend_from_slice(&(0x28u32 + sfo.len() as u32).to_le_bytes());
        data.resize(0x28, 0);
        data.extend_from_slice(&sfo);
        fs::write(&pbp, data).unwrap();
        assert_eq!(pbp_title(&pbp).as_deref(), Some("Homebrew PSP"));
    }

    #[test]
    fn scans_full_structure() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        let vita = root.join("app").join("PCSX00001");
        fs::create_dir_all(vita.join("sce_sys")).unwrap();
        fs::write(vita.join("eboot.bin"), vec![0u8; 1000]).unwrap();
        let sfo = build_sfo(&[("TITLE", "Juego Vita"), ("TITLE_ID", "PCSX00001")]);
        fs::write(vita.join("sce_sys").join("param.sfo"), &sfo).unwrap();
        let expected_vita_size = 1000 + sfo.len() as u64;

        let dlc = root.join("addcont").join("PCSX00001");
        fs::create_dir_all(&dlc).unwrap();
        fs::write(dlc.join("dlc.bin"), vec![0u8; 500]).unwrap();

        let orphan = root.join("addcont").join("PCSX99999");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("x.bin"), b"x").unwrap();

        let iso_dir = root.join("pspemu").join("ISO");
        fs::create_dir_all(&iso_dir).unwrap();
        build_iso(&iso_dir.join("God of War.iso"), "God of War");

        let hb = root
            .join("pspemu")
            .join("PSP")
            .join("GAME")
            .join("MiHomebrew");
        fs::create_dir_all(&hb).unwrap();
        fs::write(hb.join("EBOOT.PBP"), b"\0PBP").unwrap();
        fs::create_dir_all(
            root.join("pspemu")
                .join("PSP")
                .join("GAME")
                .join("%__SCE__MiHomebrew"),
        )
        .unwrap();

        let catalog = scan(root).unwrap();
        assert_eq!(catalog.games.len(), 3, "juegos: {:?}", catalog.games);

        let vita_game = catalog
            .games
            .iter()
            .find(|g| g.kind == GameKind::Vita)
            .unwrap();
        assert_eq!(vita_game.title, "Juego Vita");
        assert_eq!(vita_game.id, "PCSX00001");
        assert_eq!(vita_game.dest_rel, PathBuf::from("app/PCSX00001"));
        assert_eq!(vita_game.size, expected_vita_size);
        assert_eq!(vita_game.dlc_size, 500);
        assert!(vita_game.dlc.is_some());

        let iso_game = catalog
            .games
            .iter()
            .find(|g| g.kind == GameKind::PspIso)
            .unwrap();
        assert_eq!(iso_game.title, "God of War");
        assert_eq!(
            iso_game.dest_rel,
            PathBuf::from("pspemu/ISO/God of War.iso")
        );

        let homebrew = catalog
            .games
            .iter()
            .find(|g| g.kind == GameKind::PspHomebrew)
            .unwrap();
        assert_eq!(homebrew.title, "MiHomebrew");
        assert!(catalog.warnings.iter().any(|w| w.contains("PCSX99999")));
    }

    #[test]
    fn detects_games_already_on_destination() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ux0");
        let dest = temp.path().join("usb");

        for name in ["GAME1", "GAME2"] {
            fs::create_dir_all(root.join("app").join(name)).unwrap();
            fs::write(root.join("app").join(name).join("eboot.bin"), b"x").unwrap();
        }
        let iso_dir = root.join("pspemu").join("ISO");
        fs::create_dir_all(&iso_dir).unwrap();
        build_iso(&iso_dir.join("PSP Game.iso"), "PSP Game");
        let hb = root.join("pspemu").join("PSP").join("GAME").join("HB");
        fs::create_dir_all(&hb).unwrap();
        fs::write(hb.join("EBOOT.PBP"), b"pbp").unwrap();

        let catalog = scan(&root).unwrap();
        assert_eq!(catalog.games.len(), 4);

        fs::create_dir_all(dest.join("app").join("GAME1")).unwrap();
        fs::write(dest.join("app").join("GAME1").join("eboot.bin"), b"x").unwrap();
        // Una carpeta vacía no cuenta como juego instalado.
        fs::create_dir_all(dest.join("app").join("GAME2")).unwrap();
        fs::create_dir_all(dest.join("pspemu").join("ISO")).unwrap();
        fs::write(dest.join("pspemu").join("ISO").join("PSP Game.iso"), b"iso").unwrap();
        let installed_hb = dest.join("pspemu").join("PSP").join("GAME").join("HB");
        fs::create_dir_all(&installed_hb).unwrap();
        fs::write(installed_hb.join("EBOOT.PBP"), b"pbp").unwrap();

        let installed = installed_games(&catalog, &dest);
        assert_eq!(installed.len(), 3);
        assert!(installed.contains(
            &catalog
                .games
                .iter()
                .position(|game| game.id == "GAME1")
                .unwrap()
        ));
        assert!(!installed.contains(
            &catalog
                .games
                .iter()
                .position(|game| game.id == "GAME2")
                .unwrap()
        ));
        assert!(installed.contains(
            &catalog
                .games
                .iter()
                .position(|game| game.kind == GameKind::PspIso)
                .unwrap()
        ));
        assert!(installed.contains(
            &catalog
                .games
                .iter()
                .position(|game| game.kind == GameKind::PspHomebrew)
                .unwrap()
        ));
    }

    #[test]
    fn finds_games_missing_from_local_library() {
        let temp = tempfile::tempdir().unwrap();
        let local_root = temp.path().join("local");
        let external_root = temp.path().join("usb");

        for name in ["GAME1", "GAME2"] {
            fs::create_dir_all(local_root.join("app").join(name)).unwrap();
            fs::write(local_root.join("app").join(name).join("eboot.bin"), b"x").unwrap();
        }
        for name in ["GAME1", "GAME2", "GAME3"] {
            fs::create_dir_all(external_root.join("app").join(name)).unwrap();
            fs::write(external_root.join("app").join(name).join("eboot.bin"), b"x").unwrap();
        }
        let local = scan(&local_root).unwrap();
        let external = scan(&external_root).unwrap();

        let missing = missing_games(&local, &external);
        assert_eq!(missing.len(), 1);
        assert_eq!(external.games[missing[0]].id, "GAME3");
        assert!(!missing_games(&local, &local).len().gt(&0));
    }

    #[test]
    fn rejects_folder_without_structure() {
        let temp = tempfile::tempdir().unwrap();
        assert!(scan(temp.path()).is_err());
        assert!(scan(&temp.path().join("no-existe")).is_err());
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }
}
