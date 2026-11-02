//! UIKit document pickers, installed as the shell's file services before the first frame.
//!
//! The shell asks for a file and gets an answer on a later frame: an open picker reports
//! `(name, bytes)` on the callback it hands over, and an export picker stages the finished file and
//! lets the user place it. Reading runs on a worker thread so a large mesh never blocks the frame.

/// Largest total import a single picker answer may hand over.
pub const MAX_IMPORT_BYTES: u64 = 512 * 1024 * 1024;

/// Longest file name kept from a provider (a name longer than this is truncated at the stem).
pub const MAX_NAME_LEN: usize = 120;

/// The app's own folder under Application Support, inside the app container: the only place a
/// file the app writes on its own behalf can live.
pub fn support_dir(home: Option<&std::ffi::OsStr>) -> Result<std::path::PathBuf, String> {
    let home = home.filter(|home| !home.is_empty()).ok_or("iOS app container HOME is unavailable")?;
    Ok(std::path::PathBuf::from(home).join("Library/Application Support/ai.storyteller.texturecraft"))
}

/// The folder the app stages exports in, inside the app container.
pub fn export_dir(home: Option<&std::ffi::OsStr>) -> Result<std::path::PathBuf, String> {
    Ok(support_dir(home)?.join("exports"))
}

/// A name the app may write: the provider's last path component, with directories, empty names and
/// over-long names handled. Keeps the extension so the picker shows the right kind.
pub fn safe_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = last.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim_matches([' ', '.'].as_ref());
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return "untitled.tcraft".into();
    }
    if cleaned.chars().count() <= MAX_NAME_LEN {
        return cleaned.to_string();
    }
    let (stem, ext) = match cleaned.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.chars().count() <= 8 => (stem, format!(".{ext}")),
        _ => (cleaned, String::new()),
    };
    let keep = MAX_NAME_LEN.saturating_sub(ext.chars().count());
    let stem: String = stem.chars().take(keep.saturating_sub(1)).collect();
    format!("{stem}\u{2026}{ext}")
}

/// Read at most `limit` bytes of one file, refusing anything bigger or not a plain file.
pub fn bounded_read(path: &std::path::Path, limit: u64) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > limit {
        return Err(format!("{} is {} bytes, over the {limit} byte import limit", path.display(), meta.len()));
    }
    std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// The import limit left for the files after the ones already read.
pub fn remaining_limit(total_read: u64) -> Result<u64, String> {
    MAX_IMPORT_BYTES.checked_sub(total_read).ok_or("Selected documents exceed the import limit".into())
}

#[cfg(target_os = "ios")]
#[allow(unsafe_code)]
mod native;
#[cfg(target_os = "ios")]
pub use native::install;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_dir_stays_inside_the_app_container() {
        let home = std::ffi::OsStr::new("/private/app-container");
        assert_eq!(
            support_dir(Some(home)).unwrap(),
            std::path::PathBuf::from("/private/app-container/Library/Application Support/ai.storyteller.texturecraft")
        );
        assert_eq!(
            export_dir(Some(home)).unwrap(),
            std::path::PathBuf::from("/private/app-container/Library/Application Support/ai.storyteller.texturecraft/exports")
        );
    }

    #[test]
    fn a_missing_or_empty_container_is_an_error() {
        assert!(export_dir(None).is_err());
        assert!(export_dir(Some(std::ffi::OsStr::new(""))).is_err());
    }

    #[test]
    fn a_provider_name_never_escapes_the_stage() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("/private/var/f/48/Hello.tcraft"), "Hello.tcraft");
        assert_eq!(safe_name("C:\\Users\\sam\\Cat.glb"), "Cat.glb");
    }

    #[test]
    fn an_unusable_name_falls_back_to_a_project() {
        assert_eq!(safe_name(""), "untitled.tcraft");
        assert_eq!(safe_name(".."), "untitled.tcraft");
        assert_eq!(safe_name("..."), "untitled.tcraft");
    }

    #[test]
    fn a_long_name_keeps_its_extension() {
        let long = format!("{}{}", "a".repeat(300), ".png");
        let kept = safe_name(&long);
        assert!(kept.chars().count() <= MAX_NAME_LEN, "{kept}");
        assert!(kept.ends_with(".png"));
    }

    #[test]
    fn a_long_name_without_an_extension_is_still_capped() {
        let kept = safe_name(&"b".repeat(400));
        assert!(kept.chars().count() <= MAX_NAME_LEN);
    }

    #[test]
    fn oversize_and_missing_files_are_refused() {
        let dir = std::env::temp_dir().join(format!("texturecraft-files-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.png");
        std::fs::write(&small, b"png").unwrap();
        assert_eq!(bounded_read(&small, 16).unwrap(), b"png");
        assert!(bounded_read(&small, 2).is_err());
        assert!(bounded_read(&dir.join("missing.png"), 16).is_err());
        assert!(bounded_read(&dir, 16).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_import_budget_only_shrinks() {
        assert_eq!(remaining_limit(0).unwrap(), MAX_IMPORT_BYTES);
        assert_eq!(remaining_limit(MAX_IMPORT_BYTES - 10).unwrap(), 10);
        assert!(remaining_limit(MAX_IMPORT_BYTES + 1).is_err());
    }
}
