//! Local preferences. Context keys remain the original kubeconfig identities.
use beacon_kube::{
    ClusterId,
    connection::{ConnectionOptions, MetricsSource, Proxy},
};
use gpui_kit::{
    component::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode},
    *,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, LazyLock},
};

pub(crate) fn color_roles() -> &'static [String] {
    static ROLES: LazyLock<Vec<String>> = LazyLock::new(|| {
        let colors = serde_json::to_value(ThemeConfigColors::default())
            .expect("theme color schema is serializable");
        let mut roles: Vec<_> = colors
            .as_object()
            .expect("theme colors form an object")
            .keys()
            .cloned()
            .collect();
        roles.sort_unstable();
        roles
    });
    &ROLES
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "name", rename_all = "snake_case")]
pub(crate) enum Appearance {
    #[default]
    Light,
    Dark,
    Custom(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct CustomTheme {
    pub name: String,
    pub dark: bool,
    pub font_family: String,
    pub font_size: f32,
    pub mono_font_family: String,
    pub mono_font_size: f32,
    pub colors: BTreeMap<String, String>,
}
impl Default for CustomTheme {
    fn default() -> Self {
        Self {
            name: "My theme".into(),
            dark: false,
            font_family: ".SystemUIFont".into(),
            font_size: 16.,
            mono_font_family: if cfg!(target_os = "macos") {
                "Menlo"
            } else if cfg!(target_os = "windows") {
                "Consolas"
            } else {
                "DejaVu Sans Mono"
            }
            .into(),
            mono_font_size: 13.,
            colors: BTreeMap::new(),
        }
    }
}
impl CustomTheme {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.len() > 80 {
            return Err("Theme name must contain 1–80 characters.".into());
        }
        if self.font_family.trim().is_empty() || self.mono_font_family.trim().is_empty() {
            return Err("Choose an installed font family.".into());
        }
        if !self.font_size.is_finite()
            || !(10. ..=30.).contains(&self.font_size)
            || !self.mono_font_size.is_finite()
            || !(10. ..=30.).contains(&self.mono_font_size)
        {
            return Err("Font sizes must be between 10 and 30 px.".into());
        }
        for (role, color) in &self.colors {
            if color_roles().binary_search(role).is_err() {
                return Err(format!("Unknown theme color: {role}"));
            }
            if !color.starts_with('#')
                || !matches!(color.len(), 4 | 7 | 9)
                || !color[1..].bytes().all(|c| c.is_ascii_hexdigit())
            {
                return Err(format!("{role}: use a hex color such as #3B82F6."));
            }
        }
        Ok(())
    }
    fn config(&self, base: &Theme) -> ThemeConfig {
        let mut config = if self.dark {
            base.dark_theme.as_ref().clone()
        } else {
            base.light_theme.as_ref().clone()
        };
        config.name = self.name.clone().into();
        config.mode = if self.dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        config.font_family = Some(self.font_family.clone().into());
        config.font_size = Some(self.font_size);
        config.mono_font_family = Some(self.mono_font_family.clone().into());
        config.mono_font_size = Some(self.mono_font_size);
        let mut colors = serde_json::to_value(&config.colors).unwrap_or_default();
        for (role, value) in &self.colors {
            colors[role] = serde_json::Value::String(value.clone());
        }
        if let Ok(colors) = serde_json::from_value(colors) {
            config.colors = colors;
        }
        config
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub(crate) enum ClusterIcon {
    #[default]
    Kubernetes,
    Server,
    Cloud,
    Workloads,
    Storage,
    Shield,
    Custom(PathBuf),
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ClusterSettings {
    pub alias: String,
    pub icon: ClusterIcon,
    /// None inherits the global proxy. Direct explicitly overrides it.
    pub proxy: Option<Proxy>,
    pub metrics: MetricsSource,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub appearance: Appearance,
    pub themes: BTreeMap<String, CustomTheme>,
    pub proxy: Proxy,
    pub clusters: BTreeMap<String, ClusterSettings>,
}
impl Preferences {
    pub fn connection(&self, id: &ClusterId) -> ConnectionOptions {
        let cluster = self.clusters.get(id.as_str());
        ConnectionOptions {
            proxy: cluster
                .and_then(|c| c.proxy.clone())
                .unwrap_or_else(|| self.proxy.clone()),
            metrics: cluster.map(|c| c.metrics.clone()).unwrap_or_default(),
        }
    }
    fn validate(&self) -> Result<(), String> {
        self.proxy.validate()?;
        for theme in self.themes.values() {
            theme.validate()?;
        }
        if let Appearance::Custom(name) = &self.appearance
            && !self.themes.contains_key(name)
        {
            return Err("Selected custom theme is missing.".into());
        }
        for c in self.clusters.values() {
            ConnectionOptions {
                proxy: c.proxy.clone().unwrap_or_default(),
                metrics: c.metrics.clone(),
            }
            .validate()?;
        }
        Ok(())
    }
}

pub(crate) struct Changed(pub Option<ClusterId>);
pub(crate) struct State {
    pub preferences: Preferences,
    pub directory: PathBuf,
    pub load_error: Option<String>,
    base: Theme,
}
impl EventEmitter<Changed> for State {}
struct Store(Entity<State>);
impl Global for Store {}
pub(crate) fn store(cx: &App) -> Entity<State> {
    cx.global::<Store>().0.clone()
}
pub(crate) fn init(cx: &mut App) {
    let directory = std::env::var_os("BEACON_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            directories::ProjectDirs::from("dev", "beacon", "Beacon")
                .expect("config directory")
                .config_dir()
                .to_path_buf()
        });
    let result = read(&directory);
    let (preferences, load_error) = match result {
        Ok(p) => (p, None),
        Err(e) => (Preferences::default(), Some(e)),
    };
    let base = Theme::global(cx).clone();
    let state = cx.new(|_| State {
        preferences,
        directory,
        load_error,
        base,
    });
    cx.set_global(Store(state));
    apply(None, cx);
}
fn read(directory: &Path) -> Result<Preferences, String> {
    let path = directory.join("settings.json");
    if !path.exists() {
        return Ok(Preferences::default());
    }
    let data = std::fs::read(path).map_err(|_| "Could not read saved preferences.".to_string())?;
    let preferences: Preferences = serde_json::from_slice(&data).map_err(|_| {
        "Saved preferences are invalid. The original file has been preserved.".to_string()
    })?;
    preferences.validate()?;
    Ok(preferences)
}
pub(crate) fn save(
    preferences: Preferences,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    preferences.validate()?;
    let state = store(cx);
    let directory = state.read(cx).directory.clone();
    // Persist before updating the live settings, so failed writes never look saved.
    write(&directory, &preferences)?;
    state.update(cx, |state, cx| {
        state.preferences = preferences;
        state.load_error = None;
        cx.emit(Changed(None));
        cx.notify();
    });
    apply(Some(window), cx);
    cx.refresh_windows();
    Ok(())
}
fn write(directory: &Path, preferences: &Preferences) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let data = serde_json::to_vec_pretty(preferences).map_err(|e| e.to_string())?;
    let themes = directory.join("themes");
    std::fs::create_dir_all(&themes).map_err(|e| e.to_string())?;
    for (name, theme) in &preferences.themes {
        use sha2::{Digest, Sha256};
        let filename = format!("{:x}.json", Sha256::digest(name.as_bytes()));
        atomic_write(
            &themes.join(filename),
            &serde_json::to_vec_pretty(theme).map_err(|e| e.to_string())?,
        )?;
    }
    atomic_write(&directory.join("settings.json"), &data)?;
    Ok(())
}
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().ok_or("No settings directory.")?)
        .map_err(|e| e.to_string())?;
    temp.write_all(data)
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    temp.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
pub(crate) fn apply(window: Option<&mut Window>, cx: &mut App) {
    let state = store(cx);
    let state = state.read(cx);
    let base = state.base.clone();
    let appearance = state.preferences.appearance.clone();
    let custom = if let Appearance::Custom(name) = &appearance {
        state.preferences.themes.get(name).cloned()
    } else {
        None
    };
    cx.set_global(base.clone());
    let mode = if let Some(custom) = custom {
        let config = Rc::new(custom.config(&base));
        let mode = config.mode;
        Theme::global_mut(cx).apply_config(&config);
        mode
    } else if appearance == Appearance::Dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, window, cx);
    Theme::sync_base(cx);
}
pub(crate) fn toggle(window: &mut Window, cx: &mut App) {
    let mut p = store(cx).read(cx).preferences.clone();
    p.appearance = if Theme::global(cx).is_dark() {
        Appearance::Light
    } else {
        Appearance::Dark
    };
    if let Err(error) = save(p, window, cx) {
        tracing::warn!(%error,"could not save theme");
    }
}
pub(crate) fn icon(icon: &ClusterIcon) -> gpui_kit::component::Icon {
    use gpui_kit::component::Icon;
    match icon {
        ClusterIcon::Kubernetes => crate::icons::kubernetes(),
        ClusterIcon::Server => crate::icons::section(&crate::catalog::Section::Builtin(
            crate::catalog::Category::Cluster,
        )),
        ClusterIcon::Cloud => Icon::default().data(include_bytes!("../assets/cloud.svg")),
        ClusterIcon::Workloads => crate::icons::section(&crate::catalog::Section::Builtin(
            crate::catalog::Category::Workloads,
        )),
        ClusterIcon::Storage => crate::icons::section(&crate::catalog::Section::Builtin(
            crate::catalog::Category::Storage,
        )),
        ClusterIcon::Shield => crate::icons::section(&crate::catalog::Section::Builtin(
            crate::catalog::Category::AccessControl,
        )),
        ClusterIcon::Custom(path) => custom_icon_data(path)
            .map_or_else(crate::icons::kubernetes, |data| Icon::default().data(&data)),
    }
}

thread_local! {
    // Imported filenames are content hashes, so the bytes cannot change in place.
    static ICON_DATA: RefCell<BTreeMap<PathBuf, Arc<[u8]>>> = RefCell::default();
}
fn custom_icon_data(path: &Path) -> Option<Arc<[u8]>> {
    ICON_DATA.with(|cache| {
        if let Some(data) = cache.borrow().get(path) {
            return Some(data.clone());
        }
        if std::fs::metadata(path).ok()?.len() > 512 * 1024 {
            return None;
        }
        let data: Arc<[u8]> = std::fs::read(path).ok()?.into();
        if !std::str::from_utf8(&data).ok()?.contains("<svg") {
            return None;
        }
        cache.borrow_mut().insert(path.to_path_buf(), data.clone());
        Some(data)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        Appearance, ClusterSettings, CustomTheme, Preferences, custom_icon_data, read, write,
    };
    use beacon_kube::{ClusterId, connection::Proxy};
    use gpui_kit::{component::Theme, px, rgb};
    use std::rc::Rc;
    #[test]
    fn cluster_proxy_overrides_global_and_alias_does_not_change_identity() {
        let id = ClusterId::new("original");
        let mut p = Preferences {
            proxy: Proxy::Custom("http://global:8080".into()),
            ..Default::default()
        };
        p.clusters.insert(
            id.to_string(),
            ClusterSettings {
                alias: "Production".into(),
                proxy: Some(Proxy::Direct),
                ..Default::default()
            },
        );
        assert_eq!(p.connection(&id).proxy, Proxy::Direct);
        p.clusters.get_mut(id.as_str()).unwrap().proxy = None;
        assert_eq!(p.connection(&id).proxy, p.proxy);
        assert_eq!(id.as_str(), "original");
    }
    #[test]
    fn round_trip_persists_themes_and_cluster_settings() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Preferences::default();
        let mut t = CustomTheme::default();
        t.colors
            .insert("button.primary.foreground".into(), "#00FF00".into());
        p.themes.insert(t.name.clone(), t.clone());
        p.appearance = Appearance::Custom(t.name.clone());
        write(dir.path(), &p).unwrap();
        let read = read(dir.path()).unwrap();
        assert_eq!(read.appearance, p.appearance);
        assert_eq!(read.themes[&t.name].colors, t.colors);
        assert_eq!(
            std::fs::read_dir(dir.path().join("themes"))
                .unwrap()
                .count(),
            1
        );
    }
    #[test]
    fn invalid_colors_fonts_and_missing_themes_are_rejected() {
        let mut t = CustomTheme::default();
        t.colors.insert("not_a_token".into(), "#00FF00".into());
        assert!(t.validate().is_err());
        t.colors.clear();
        t.font_size = 0.;
        assert!(t.validate().is_err());
        let p = Preferences {
            appearance: Appearance::Custom("missing".into()),
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn custom_theme_applies_button_colors_and_fonts() {
        let base = Theme::default();
        let mut custom = CustomTheme {
            font_family: "Example font".into(),
            font_size: 18.,
            mono_font_size: 14.,
            ..Default::default()
        };
        custom
            .colors
            .insert("button.primary.foreground".into(), "#00FF00".into());
        custom.validate().unwrap();
        let mut theme = base.clone();
        theme.apply_config(&Rc::new(custom.config(&base)));
        assert_eq!(theme.button_primary_foreground, rgb(0x00ff00).into());
        assert_eq!(theme.font_family.as_ref(), "Example font");
        assert_eq!(theme.font_size, px(18.));
        assert_eq!(theme.mono_font_size, px(14.));
        assert_eq!(base.font_size, Theme::default().font_size);
    }

    #[test]
    fn invalid_preferences_are_not_modified_on_read() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let original = b"{invalid json";
        std::fs::write(&path, original).unwrap();
        assert!(read(directory.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn imported_svg_uses_cached_bytes_and_missing_files_fall_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("icon.svg");
        assert!(custom_icon_data(&path).is_none());
        let svg = include_bytes!("../assets/cloud.svg");
        std::fs::write(&path, svg).unwrap();
        let data = custom_icon_data(&path).unwrap();
        assert_eq!(data.as_ref(), svg);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(custom_icon_data(&path).unwrap().as_ref(), svg);
    }
}
