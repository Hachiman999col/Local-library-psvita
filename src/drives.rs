//! Detección de unidades montadas (extraíbles primero) y atajos del explorador.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Drive {
    pub mount: PathBuf,
    pub label: String,
    pub device: String,
    pub removable: bool,
    pub free: u64,
    pub total: u64,
}

pub fn detect_drives() -> Vec<Drive> {
    let mut found = platform::detect();
    found.sort_by(|a, b| {
        b.removable
            .cmp(&a.removable)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    found.dedup_by(|a, b| a.mount == b.mount);
    found
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(unix)]
mod platform {
    use super::Drive;
    use std::fs;

    pub fn detect() -> Vec<Drive> {
        let mut drives = Vec::new();

        #[cfg(target_os = "linux")]
        {
            let Ok(mounts) = fs::read_to_string("/proc/mounts") else {
                return drives;
            };
            for line in mounts.lines() {
                let mut parts = line.split_whitespace();
                let (Some(device), Some(mountpoint), Some(fstype)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                if !device.starts_with("/dev/") || !is_real_filesystem(fstype) {
                    continue;
                }
                let mountpoint = unescape(mountpoint);
                let mount = std::path::PathBuf::from(&mountpoint);
                if mount == std::path::Path::new("/boot")
                    || mount == std::path::Path::new("/boot/efi")
                {
                    continue;
                }

                let removable = is_removable(device);
                let user_visible = mountpoint.starts_with("/run/media/")
                    || mountpoint.starts_with("/media/")
                    || mountpoint.starts_with("/mnt/");
                if !removable && !user_visible {
                    continue;
                }

                drives.push(Drive {
                    label: mount
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string)
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| mountpoint.clone()),
                    free: fs4::available_space(&mount).unwrap_or(0),
                    total: fs4::total_space(&mount).unwrap_or(0),
                    device: unescape(device),
                    mount,
                    removable,
                });
            }
        }

        #[cfg(target_os = "macos")]
        {
            if let Ok(entries) = fs::read_dir("/Volumes") {
                for entry in entries.flatten() {
                    let mount = entry.path();
                    if !mount.is_dir() {
                        continue;
                    }
                    if mount == std::path::Path::new("/Volumes") {
                        continue;
                    }
                    drives.push(Drive {
                        label: entry.file_name().to_string_lossy().to_string(),
                        free: fs4::available_space(&mount).unwrap_or(0),
                        total: fs4::total_space(&mount).unwrap_or(0),
                        device: mount.display().to_string(),
                        mount,
                        removable: true,
                    });
                }
            }
        }

        drives
    }

    #[cfg(target_os = "linux")]
    fn is_real_filesystem(fstype: &str) -> bool {
        if fstype.starts_with("fuse") {
            return true;
        }
        matches!(
            fstype,
            "ext2"
                | "ext3"
                | "ext4"
                | "xfs"
                | "btrfs"
                | "vfat"
                | "fat"
                | "exfat"
                | "ntfs"
                | "ntfs3"
                | "msdos"
                | "f2fs"
                | "hfsplus"
                | "apfs"
                | "iso9660"
                | "udf"
        )
    }

    #[cfg(target_os = "linux")]
    fn is_removable(device: &str) -> bool {
        let Some(name) = std::path::Path::new(device)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            return false;
        };
        let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
        for candidate in [name, base] {
            if candidate.is_empty() {
                continue;
            }
            for path in [
                format!("/sys/class/block/{candidate}/removable"),
                format!("/sys/block/{candidate}/removable"),
            ] {
                if fs::read_to_string(path)
                    .map(|value| value.trim() == "1")
                    .unwrap_or(false)
                {
                    return true;
                }
            }
        }
        false
    }

    #[cfg(target_os = "linux")]
    fn unescape(value: &str) -> String {
        let bytes = value.as_bytes();
        let mut out = String::with_capacity(value.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'\\' && index + 3 < bytes.len() {
                let digits = &value[index + 1..index + 4];
                if let Ok(code) = u8::from_str_radix(digits, 8) {
                    out.push(code as char);
                    index += 4;
                    continue;
                }
            }
            out.push(bytes[index] as char);
            index += 1;
        }
        out
    }
}

#[cfg(windows)]
mod platform {
    use super::Drive;
    use std::path::PathBuf;

    pub fn detect() -> Vec<Drive> {
        let mut drives = Vec::new();
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            let mount = PathBuf::from(&root);
            if !mount.is_dir() {
                continue;
            }
            drives.push(Drive {
                label: root.clone(),
                free: fs4::available_space(&mount).unwrap_or(0),
                total: fs4::total_space(&mount).unwrap_or(0),
                device: root,
                mount,
                removable: false,
            });
        }
        drives
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_at_least_the_project_volume() {
        // En este entorno la unidad del proyecto es un montaje real.
        let drives = detect_drives();
        if cfg!(target_os = "linux") {
            assert!(
                !drives.is_empty(),
                "se esperaba al menos una unidad montada"
            );
            assert!(drives.iter().all(|drive| drive.mount.is_dir()));
        }
    }
}
