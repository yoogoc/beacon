//! Keep user-selected kubeconfig locations accessible across sandboxed launches.
use gpui_kit::{App, Global};
use objc2::{rc::Retained, runtime::Bool};
use objc2_foundation::{
    NSData, NSString, NSURL, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions,
};
use std::{collections::BTreeMap, path::PathBuf};

struct Access(Retained<NSURL>);
impl Drop for Access {
    fn drop(&mut self) {
        // SAFETY: Access is constructed only after a successful start call.
        unsafe { self.0.stopAccessingSecurityScopedResource() };
    }
}
struct FileAccess {
    path: PathBuf,
    bookmarks: BTreeMap<PathBuf, Vec<u8>>,
    active: BTreeMap<PathBuf, Access>,
}
impl Global for FileAccess {}

fn bookmark(url: &NSURL) -> Result<Vec<u8>, String> {
    url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
        NSURLBookmarkCreationOptions::WithSecurityScope,
        None,
        None,
    )
    .map(|data| data.to_vec())
    .map_err(|error| format!("Could not save file access. Choose the location again. {error}"))
}

fn resolve(bytes: &[u8]) -> Result<(Access, bool), String> {
    let mut stale = Bool::NO;
    // SAFETY: stale points to a live Bool for the duration of this call.
    let url = unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            &NSData::with_bytes(bytes),
            NSURLBookmarkResolutionOptions::WithSecurityScope
                | NSURLBookmarkResolutionOptions::WithoutUI,
            None,
            &mut stale,
        )
    }
    .map_err(|error| {
        format!("Could not restore file access. Choose the location again. {error}")
    })?;
    // SAFETY: the URL was resolved with security scope and remains retained.
    if !unsafe { url.startAccessingSecurityScopedResource() } {
        return Err("Could not access the selected location. Choose it again.".into());
    }
    Ok((Access(url), stale.as_bool()))
}

pub(crate) fn init(cx: &mut App) {
    let path = crate::settings::store(cx)
        .read(cx)
        .directory
        .join("file-access.json");
    let bookmarks = match std::fs::read(&path) {
        Ok(data) => serde_json::from_slice::<BTreeMap<PathBuf, Vec<u8>>>(&data)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "could not read saved file access; reselect kubeconfig locations");
                BTreeMap::new()
            }),
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, "could not read saved file access");
            }
            BTreeMap::new()
        }
    };
    let mut state = FileAccess {
        path,
        bookmarks,
        active: BTreeMap::new(),
    };
    let mut refreshed = false;
    for (path, bytes) in &mut state.bookmarks {
        match resolve(bytes) {
            Ok((access, stale)) => {
                if stale && let Ok(data) = bookmark(&access.0) {
                    *bytes = data;
                    refreshed = true;
                }
                state.active.insert(path.clone(), access);
            }
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "file access expired; reselect the kubeconfig location")
            }
        }
    }
    if refreshed && let Err(error) = state.save() {
        tracing::warn!(%error, "could not refresh saved file access");
    }
    cx.set_global(state);
}

impl FileAccess {
    fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(self.path.parent().ok_or("No settings directory.")?)
            .map_err(|error| error.to_string())?;
        crate::settings::atomic_write(
            &self.path,
            &serde_json::to_vec(&self.bookmarks).map_err(|error| error.to_string())?,
        )
    }
}

/// Call immediately after the native Open panel grants access to these paths.
pub(crate) fn remember(paths: &[PathBuf], cx: &mut App) -> Result<(), String> {
    let state = cx.global_mut::<FileAccess>();
    for path in paths {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let data = bookmark(&url)?;
        // GPUI returns paths. Resolve the bookmark to regain a scoped NSURL.
        let (access, _) = resolve(&data)?;
        state.bookmarks.insert(path.clone(), data);
        state.active.insert(path.clone(), access);
    }
    state.save()
}
