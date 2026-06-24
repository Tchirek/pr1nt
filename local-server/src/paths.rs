use std::path::{Path, PathBuf};

pub(crate) fn resolve_relative_path(base_dir: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return path;
    }
    base_dir.join(path)
}

pub(crate) fn resolve_command_path(base_dir: &Path, raw: &str) -> String {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return path.display().to_string();
    }

    if raw.starts_with('.')
        || raw.contains(std::path::MAIN_SEPARATOR)
        || raw.contains('/')
        || raw.contains('\\')
    {
        return base_dir.join(path).display().to_string();
    }

    raw.to_owned()
}
