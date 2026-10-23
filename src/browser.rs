//! Explorador de carpetas simple, sin dependencias del sistema gráfico.

use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct FolderBrowser {
    pub current: PathBuf,
    pub entries: Vec<Entry>,
    pub error: Option<String>,
    pub show_hidden: bool,
}

impl FolderBrowser {
    pub fn new(start: PathBuf) -> Self {
        let current = if start.is_dir() {
            start
        } else {
            start.parent().map(PathBuf::from).unwrap_or(start)
        };
        let mut browser = Self {
            current,
            entries: Vec::new(),
            error: None,
            show_hidden: false,
        };
        browser.refresh();
        browser
    }

    pub fn navigate(&mut self, path: PathBuf) {
        if !path.is_dir() {
            return;
        }
        self.current = path;
        self.refresh();
    }

    pub fn go_up(&mut self) {
        if let Some(parent) = self.current.parent().map(PathBuf::from) {
            self.navigate(parent);
        }
    }

    pub fn refresh(&mut self) {
        self.entries.clear();
        self.error = None;
        match fs::read_dir(&self.current) {
            Ok(read_dir) => {
                for entry in read_dir.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !self.show_hidden && name.starts_with('.') {
                        continue;
                    }
                    let path = entry.path();
                    // Se sigue el enlace simbólico para permitir accesos directos a carpetas.
                    if path.is_dir() {
                        self.entries.push(Entry { name, path });
                    }
                }
                self.entries
                    .sort_by_cached_key(|entry| entry.name.to_lowercase());
            }
            Err(error) => {
                self.error = Some(format!(
                    "No se pudo leer «{}»: {error}",
                    self.current.display()
                ));
            }
        }
    }

    pub fn set_show_hidden(&mut self, show: bool) {
        if self.show_hidden != show {
            self.show_hidden = show;
            self.refresh();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigates_and_lists_only_directories() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("carpeta")).unwrap();
        fs::create_dir(temp.path().join(".oculta")).unwrap();
        fs::write(temp.path().join("archivo.txt"), b"x").unwrap();

        let mut browser = FolderBrowser::new(temp.path().to_path_buf());
        assert_eq!(browser.entries.len(), 1);
        assert_eq!(browser.entries[0].name, "carpeta");

        browser.set_show_hidden(true);
        assert_eq!(browser.entries.len(), 2);

        browser.navigate(temp.path().join("carpeta"));
        assert_eq!(browser.current, temp.path().join("carpeta"));
        assert!(browser.entries.is_empty());

        browser.go_up();
        assert_eq!(browser.current, temp.path());
    }

    #[test]
    fn survives_invalid_paths() {
        let temp = tempfile::tempdir().unwrap();
        let mut browser = FolderBrowser::new(temp.path().join("no-existe").join("tampoco"));
        assert!(browser.error.is_some());
        browser.navigate(temp.path().to_path_buf());
        assert!(browser.error.is_none());
    }
}
