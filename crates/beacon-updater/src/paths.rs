//! Application storage paths and one-time migration during an upgrade.
use directories::ProjectDirs;
#[cfg(windows)]
use std::os::windows::fs::{FileTypeExt as _, MetadataExt as _};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn config_dir() -> io::Result<PathBuf> {
    directory(ProjectDirs::config_dir)
}

pub fn data_local_dir() -> io::Result<PathBuf> {
    directory(ProjectDirs::data_local_dir)
}

fn directory(select: fn(&ProjectDirs) -> &Path) -> io::Result<PathBuf> {
    let current = ProjectDirs::from("top", "yoogo", "Beacon")
        .ok_or_else(|| io::Error::other("Could not determine the application directory"))?;
    let destination = select(&current).to_path_buf();
    // These components are only for reading settings written by older versions.
    if let Some(previous) = ProjectDirs::from("dev", "beacon", "Beacon") {
        let source = select(&previous);
        if let Err(error) = migrate(source, &destination) {
            // Keep existing preferences usable if permissions or a locked file
            // prevent migration. Retry on the next launch, preserving all data.
            tracing::warn!(%error, source = %source.display(), destination = %destination.display(), "could not migrate application data; using the previous directory");
            return Ok(source.to_path_buf());
        }
    }
    Ok(destination)
}

fn migrate(source: &Path, destination: &Path) -> io::Result<()> {
    if source == destination {
        return Ok(());
    }
    match fs::metadata(destination) {
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Application directory is not a folder",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match fs::metadata(source) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Previous application directory is not a folder",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("No application directory parent"))?;
    fs::create_dir_all(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".beacon-migrate-")
        .tempdir_in(parent)?;
    copy_tree(source, staging.path(), source, destination)?;
    match fs::rename(staging.path(), destination) {
        Ok(()) => Ok(()),
        // Another instance may have finished migration while this one copied.
        Err(_) if destination.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    old_root: &Path,
    new_root: &Path,
) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source = entry.path();
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        // Do not traverse Windows junctions or other non-symlink reparse
        // points into external trees. Preserve the old directory on failure.
        #[cfg(windows)]
        if !kind.is_symlink() && entry.metadata()?.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unsupported reparse point in the previous application directory",
            ));
        }
        if kind.is_symlink() {
            let link = fs::read_link(&source)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(link, target)?;
            #[cfg(windows)]
            if kind.is_symlink_dir() {
                std::os::windows::fs::symlink_dir(link, target)?;
            } else {
                std::os::windows::fs::symlink_file(link, target)?;
            }
        } else if kind.is_dir() {
            fs::create_dir(&target)?;
            copy_tree(&source, &target, old_root, new_root)?;
        } else if kind.is_file() {
            fs::copy(&source, &target)?;
            if source
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                // Rebase owned icon/source paths, preserving invalid JSON so
                // the existing preferences error handling can report it.
                if let Ok(mut value) = serde_json::from_slice(&fs::read(&target)?)
                    && rebase_paths(&mut value, old_root, new_root)
                {
                    fs::write(&target, serde_json::to_vec_pretty(&value)?)?;
                }
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unsupported file in the previous application directory",
            ));
        }
    }
    Ok(())
}

fn rebase_paths(value: &mut serde_json::Value, source: &Path, destination: &Path) -> bool {
    match value {
        serde_json::Value::String(text) => {
            if let Ok(relative) = Path::new(text).strip_prefix(source) {
                *text = destination.join(relative).to_string_lossy().into_owned();
                true
            } else {
                false
            }
        }
        serde_json::Value::Array(values) => {
            let mut changed = false;
            for value in values {
                changed |= rebase_paths(value, source, destination);
            }
            changed
        }
        serde_json::Value::Object(values) => {
            let mut changed = false;
            for value in values.values_mut() {
                changed |= rebase_paths(value, source, destination);
            }
            changed
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_state_atomically_rebases_owned_paths_and_preserves_backup() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("previous");
        let new = temp.path().join("current");
        for directory in ["icons", "themes", "kubeconfigs", "logs"] {
            fs::create_dir_all(old.join(directory)).unwrap();
            fs::write(old.join(directory).join("entry"), directory).unwrap();
        }
        let external = temp.path().join("previous-other/config");
        let settings = serde_json::to_vec(&serde_json::json!({
            "icon": old.join("icons/entry"),
            "sources": [{"path": old.join("kubeconfigs/entry")}],
            "external": external,
        }))
        .unwrap();
        fs::write(old.join("settings.json"), &settings).unwrap();
        migrate(&old, &new).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(new.join("settings.json")).unwrap()).unwrap();
        assert_eq!(
            value["icon"],
            new.join("icons/entry").to_string_lossy().as_ref()
        );
        assert_eq!(
            value["sources"][0]["path"],
            new.join("kubeconfigs/entry").to_string_lossy().as_ref()
        );
        assert_eq!(value["external"], external.to_string_lossy().as_ref());
        assert_eq!(fs::read(old.join("settings.json")).unwrap(), settings);
        for directory in ["icons", "themes", "kubeconfigs", "logs"] {
            assert_eq!(
                fs::read_to_string(new.join(directory).join("entry")).unwrap(),
                directory
            );
        }
        fs::write(new.join("settings.json"), b"new preferences").unwrap();
        migrate(&old, &new).unwrap();
        assert_eq!(
            fs::read(new.join("settings.json")).unwrap(),
            b"new preferences"
        );
    }

    #[test]
    fn existing_destination_and_unchanged_platform_paths_are_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("previous");
        let new = temp.path().join("current");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&new).unwrap();
        fs::write(old.join("settings.json"), b"previous").unwrap();
        fs::write(new.join("settings.json"), b"current").unwrap();
        migrate(&old, &new).unwrap();
        migrate(&old, &old).unwrap();
        assert_eq!(fs::read(old.join("settings.json")).unwrap(), b"previous");
        assert_eq!(fs::read(new.join("settings.json")).unwrap(), b"current");
    }

    #[test]
    fn invalid_preferences_are_preserved_for_normal_error_reporting() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("previous");
        let new = temp.path().join("current");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("settings.json"), b"{invalid").unwrap();
        migrate(&old, &new).unwrap();
        assert_eq!(fs::read(new.join("settings.json")).unwrap(), b"{invalid");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_preserved_without_copying_external_trees() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("previous");
        let new = temp.path().join("current");
        let external = temp.path().join("external");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&external).unwrap();
        std::os::unix::fs::symlink(&external, old.join("linked")).unwrap();
        migrate(&old, &new).unwrap();
        assert!(
            fs::symlink_metadata(new.join("linked"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(new.join("linked")).unwrap(), external);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_copy_does_not_publish_partial_data() {
        let temp = tempfile::Builder::new()
            .prefix("bm-")
            .tempdir_in("/tmp")
            .unwrap();
        let old = temp.path().join("previous");
        let new = temp.path().join("current");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("settings.json"), b"original").unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(old.join("socket")).unwrap();
        assert!(migrate(&old, &new).is_err());
        assert!(!new.exists());
        assert_eq!(fs::read(old.join("settings.json")).unwrap(), b"original");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
