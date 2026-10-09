use crate::{Downloaded, Format, verify_file};
use anyhow::{Context, Result, bail};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Installation {
    MacApp {
        bundle: PathBuf,
        executable: PathBuf,
    },
    Windows {
        executable: PathBuf,
    },
    AppImage {
        path: PathBuf,
    },
    Managed,
}
impl Installation {
    pub fn detect() -> Self {
        let Ok(executable) = std::env::current_exe() else {
            return Self::Managed;
        };
        #[cfg(target_os = "macos")]
        if let Some(bundle) = app_bundle(&executable) {
            return Self::MacApp { bundle, executable };
        }
        #[cfg(target_os = "windows")]
        if executable
            .parent()
            .is_some_and(|dir| dir.join("uninstall.exe").is_file())
        {
            return Self::Windows { executable };
        }
        #[cfg(target_os = "linux")]
        if let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from)
            && path.is_absolute()
            && path.is_file()
        {
            return Self::AppImage { path };
        }
        let _ = executable;
        Self::Managed
    }
    pub fn can_install(&self) -> bool {
        !matches!(self, Self::Managed)
    }
    pub fn platform(&self) -> String {
        let format = match self {
            Self::MacApp { .. } => "app",
            Self::Windows { .. } => "nsis",
            Self::AppImage { .. } => "appimage",
            Self::Managed if cfg!(target_os = "macos") => "app",
            Self::Managed if cfg!(target_os = "windows") => "nsis",
            Self::Managed => "deb",
        };
        format!(
            "{}-{}-{format}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    }
    pub fn accepts(&self, format: &Format) -> bool {
        match self {
            Self::MacApp { .. } => matches!(format, Format::App),
            Self::Windows { .. } => matches!(format, Format::Nsis),
            Self::AppImage { .. } => matches!(format, Format::Appimage),
            Self::Managed if cfg!(target_os = "macos") => matches!(format, Format::App),
            Self::Managed if cfg!(target_os = "windows") => matches!(format, Format::Nsis),
            Self::Managed => matches!(format, Format::Deb),
        }
    }
}
#[cfg(any(target_os = "macos", test))]
fn app_bundle(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}
#[derive(Serialize, Deserialize)]
struct Plan {
    installation: Installation,
    package: PathBuf,
    signature: String,
    lock: PathBuf,
    receipt: PathBuf,
}
/// Keep this guard alive until the application exits. The helper cannot alter
/// the application while any GUI window still owns this lock.
pub struct PreparedInstall {
    _lock: File,
    download: Arc<Downloaded>,
    helper: PathBuf,
    plan: PathBuf,
}
impl PreparedInstall {
    pub fn download(&self) -> Arc<Downloaded> {
        self.download.clone()
    }
    pub fn launch(&self) -> Result<()> {
        let mut command = Command::new(&self.helper);
        command.arg("--beacon-update").arg(&self.plan);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(0x08000000);
        }
        let mut directory = self
            .download
            .directory
            .lock()
            .map_err(|_| anyhow::anyhow!("Update cache lock failed"))?;
        command
            .spawn()
            .context("Could not start the update helper")?;
        directory.disable_cleanup(true);
        Ok(())
    }
}
pub fn prepare_install(
    download: Arc<Downloaded>,
    installation: Installation,
) -> Result<PreparedInstall> {
    if !installation.can_install() || !installation.accepts(&download.release.asset.format) {
        bail!("This installation must be updated with its package manager");
    }
    verify_file(&download.path, &download.release.asset.signature)?;
    let directory_guard = download
        .directory
        .lock()
        .map_err(|_| anyhow::anyhow!("Update cache lock failed"))?;
    let directory = directory_guard.path();
    let helper = directory.join(if cfg!(windows) {
        "beacon-update-helper.exe"
    } else {
        "beacon-update-helper"
    });
    fs::copy(std::env::current_exe()?, &helper).context("Could not prepare the update helper")?;
    let lock_path = directory.join("exit.lock");
    let lock = File::create(&lock_path)?;
    lock.lock_exclusive()?;
    let plan = Plan {
        installation,
        package: download.path.clone(),
        signature: download.release.asset.signature.clone(),
        lock: lock_path,
        receipt: directory
            .parent()
            .context("No update cache directory")?
            .join("install-result.json"),
    };
    let plan_path = directory.join("install.json");
    fs::write(&plan_path, serde_json::to_vec(&plan)?)?;
    drop(directory_guard);
    Ok(PreparedInstall {
        _lock: lock,
        download,
        helper,
        plan: plan_path,
    })
}
/// Must be called before logging workers, Tokio or GPUI are initialized.
pub fn helper_main() -> Option<Result<()>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--beacon-update")) {
        return None;
    }
    Some((|| {
        let path = args
            .next()
            .map(PathBuf::from)
            .context("Missing update plan")?;
        let plan: Plan = serde_json::from_slice(&fs::read(&path)?)?;
        let result = apply(&plan);
        let message = match &result {
            Ok(()) => "Update installed successfully.".to_owned(),
            Err(e) => format!("Update installation failed: {e:#}"),
        };
        fs::write(
            &plan.receipt,
            serde_json::to_vec(
                &serde_json::json!({ "success": result.is_ok(), "message": message }),
            )?,
        )?;
        let parent_exited =
            File::open(&plan.lock).is_ok_and(|lock| lock.try_lock_exclusive().is_ok());
        if parent_exited && (result.is_err() || cfg!(target_os = "windows")) {
            // Installation failures keep the original app. Reopen it so the
            // next settings window can explain the failure and offer a retry.
            let _ = relaunch(&plan.installation);
        }
        if result.is_ok() {
            // Windows cannot remove the running helper; the next launch cleans
            // the cache. Unix can safely unlink it after installation.
            #[cfg(not(windows))]
            if let Some(dir) = path.parent() {
                let _ = fs::remove_dir_all(dir);
            }
        }
        result
    })())
}
fn relaunch(installation: &Installation) -> Result<()> {
    match installation {
        #[cfg(target_os = "macos")]
        Installation::MacApp { bundle, .. } => {
            Command::new("/usr/bin/open")
                .arg("-n")
                .arg(bundle)
                .spawn()?;
        }
        #[cfg(target_os = "windows")]
        Installation::Windows { executable } => {
            Command::new(executable).spawn()?;
        }
        #[cfg(target_os = "linux")]
        Installation::AppImage { path } => {
            let mut command = Command::new(path);
            for name in ["APPIMAGE", "APPDIR", "OWD", "LD_LIBRARY_PATH"] {
                command.env_remove(name);
            }
            command.spawn()?;
        }
        _ => {}
    }
    Ok(())
}
fn apply(plan: &Plan) -> Result<()> {
    let lock = File::open(&plan.lock)?;
    let deadline = Instant::now() + Duration::from_secs(120);
    while lock.try_lock_exclusive().is_err() {
        if Instant::now() >= deadline {
            bail!("Beacon did not exit; installation was cancelled");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    std::thread::sleep(Duration::from_millis(500));
    verify_file(&plan.package, &plan.signature)?;
    match &plan.installation {
        #[cfg(target_os = "macos")]
        Installation::MacApp { bundle, executable } => {
            install_app(&plan.package, bundle, executable)
        }
        #[cfg(target_os = "linux")]
        Installation::AppImage { path } => install_appimage(&plan.package, path),
        #[cfg(target_os = "windows")]
        Installation::Windows { executable } => install_windows(&plan.package, executable),
        _ => bail!("Update installation format is not supported on this platform"),
    }
}
/// Rename both ways on the destination filesystem. A failed replacement puts
/// the original application back before returning the error.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn replace(staged: &Path, target: &Path, backup: &Path) -> Result<()> {
    fs::rename(target, backup)
        .context("Could not move the existing application; check folder permissions")?;
    if let Err(error) = fs::rename(staged, target) {
        fs::rename(backup, target).context("Could not restore the original application")?;
        return Err(error)
            .context("Could not install the update; the original application was restored");
    }
    Ok(())
}
#[cfg(target_os = "macos")]
fn install_app(package: &Path, bundle: &Path, executable: &Path) -> Result<()> {
    if app_bundle(executable).as_deref() != Some(bundle) {
        bail!("Invalid macOS application location");
    }
    let parent = bundle.parent().context("No application folder")?;
    let staging = tempfile::Builder::new().prefix(".beacon-update-").tempdir_in(parent)
        .context("The application folder is not writable. Move Beacon to a writable Applications folder or install the update manually.")?;
    let archive = flate2::read::GzDecoder::new(File::open(package)?);
    let mut archive = tar::Archive::new(archive);
    // tar rejects absolute paths, parent traversal and writes through symlinks.
    archive
        .unpack(staging.path())
        .context("Invalid application archive")?;
    let apps: Vec<_> = fs::read_dir(staging.path())?
        .filter_map(|p| p.ok())
        .map(|p| p.path())
        .filter(|p| p.extension().is_some_and(|s| s == "app"))
        .collect();
    if apps.len() != 1 || !apps[0].join("Contents/MacOS/beacon").is_file() {
        bail!("Update archive does not contain one Beacon application");
    }
    let backup = staging.path().join("previous.app");
    replace(&apps[0], bundle, &backup)?;
    let status = Command::new("/usr/bin/open")
        .arg("-n")
        .arg(bundle)
        .status()?;
    if !status.success() {
        fs::remove_dir_all(bundle)?;
        fs::rename(&backup, bundle)?;
        bail!("Could not reopen Beacon; the original application was restored");
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn install_appimage(package: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let staging = tempfile::Builder::new()
        .prefix(".beacon-update-")
        .tempdir_in(target.parent().context("No AppImage folder")?)?;
    let next = staging.path().join("Beacon.AppImage");
    fs::copy(package, &next)?;
    fs::set_permissions(&next, fs::Permissions::from_mode(0o755))?;
    let backup = staging.path().join("previous.AppImage");
    replace(&next, target, &backup)?;
    // Do not inherit the old AppImage mount's runtime paths on relaunch.
    let mut command = Command::new(target);
    for name in ["APPIMAGE", "APPDIR", "OWD", "LD_LIBRARY_PATH"] {
        command.env_remove(name);
    }
    if let Err(error) = command.spawn() {
        fs::remove_file(target)?;
        fs::rename(&backup, target)?;
        return Err(error).context("Could not reopen Beacon; the original AppImage was restored");
    }
    Ok(())
}
#[cfg(target_os = "windows")]
fn install_windows(package: &Path, executable: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt as _;
    let installer = package.with_extension("exe");
    fs::copy(package, &installer)?;
    let directory = executable.parent().context("No application folder")?;
    // /P keeps the installer visible. /D must be the final argument and must
    // not be quoted. The helper relaunches after writing the install receipt.
    let status = Command::new(installer)
        .arg("/P")
        .raw_arg(format!("/D={}", directory.display()))
        .status()?;
    if !status.success() {
        bail!("Windows installer exited with {status}; reopen Beacon and retry");
    }
    Ok(())
}
/// Read the previous helper result and remove completed or stale staging
/// folders. Never remove a folder whose exit lock belongs to a live process.
pub fn read_install_result(cache: &Path) -> Option<String> {
    let receipt = cache.join("install-result.json");
    let message = fs::read(&receipt)
        .ok()
        .filter(|bytes| bytes.len() <= 16384)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value["message"].as_str().map(str::to_owned));
    if message.is_some() {
        let _ = fs::remove_file(receipt);
    }
    if let Ok(entries) = fs::read_dir(cache) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !entry.file_name().to_string_lossy().starts_with("download-") {
                continue;
            }
            let lock_path = path.join("exit.lock");
            if lock_path.exists() {
                let Ok(lock) = File::open(lock_path) else {
                    continue;
                };
                if lock.try_lock_exclusive().is_err() {
                    continue;
                }
                drop(lock);
            }
            let stale = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|elapsed| elapsed > Duration::from_secs(24 * 60 * 60));
            if path.join("install.json").exists() || stale {
                let _ = fs::remove_dir_all(path);
            }
        }
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_real_app_bundle_layouts_are_detected() {
        assert_eq!(
            app_bundle(Path::new("/Applications/Beacon.app/Contents/MacOS/beacon")),
            Some(PathBuf::from("/Applications/Beacon.app"))
        );
        assert!(app_bundle(Path::new("/tmp/beacon")).is_none());
        assert!(app_bundle(Path::new("/tmp/Beacon.app/beacon")).is_none());
    }
    #[test]
    fn cache_cleanup_preserves_a_live_exit_lock() {
        let cache = tempfile::tempdir().unwrap();
        let directory = cache.path().join("download-active");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("install.json"), b"{}").unwrap();
        let lock = File::create(directory.join("exit.lock")).unwrap();
        lock.lock_exclusive().unwrap();
        assert!(read_install_result(cache.path()).is_none());
        assert!(directory.exists());
        drop(lock);
        read_install_result(cache.path());
        assert!(!directory.exists());
    }
    #[test]
    fn successful_replacement_preserves_a_backup_until_relaunch() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("beacon");
        let staged = temp.path().join("new");
        let backup = temp.path().join("old");
        fs::write(&target, "original").unwrap();
        fs::write(&staged, "updated").unwrap();
        replace(&staged, &target, &backup).unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "updated");
        assert_eq!(fs::read_to_string(backup).unwrap(), "original");
    }
    #[test]
    fn failed_replacement_restores_the_original_application() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("beacon");
        let backup = temp.path().join("old");
        fs::write(&target, "original").unwrap();
        assert!(replace(&temp.path().join("missing"), &target, &backup).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "original");
        assert!(!backup.exists());
    }
}
