//! Application and cluster settings, presented in independent native windows.
use crate::settings::{self, Appearance, ClusterIcon, ClusterSettings, CustomTheme};
use beacon_kube::{
    ClusterId,
    connection::{MetricsSource, Prometheus, Proxy},
};
use gpui_kit::component::{Disableable as _, Selectable as _};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    component::{ActiveTheme as _, Root, TitleBar, h_flex, v_flex},
    *,
};
use std::{collections::BTreeMap, path::PathBuf, rc::Rc};

#[derive(Default)]
struct Windows {
    application: Option<WindowHandle<Root>>,
    clusters: BTreeMap<String, WindowHandle<Root>>,
    fonts: Option<Entity<FontFamilies>>,
}
impl Global for Windows {}

struct FontFamilies {
    names: Option<Rc<[String]>>,
    _load: Option<Task<()>>,
}
fn font_families(cx: &mut App) -> Entity<FontFamilies> {
    if let Some(fonts) = &cx.global::<Windows>().fonts {
        return fonts.clone();
    }
    let fonts = cx.new(|_| FontFamilies {
        names: None,
        _load: None,
    });
    cx.global_mut::<Windows>().fonts = Some(fonts.clone());
    fonts.update(cx, |fonts, cx| {
        // Enumerating system fonts can take hundreds of milliseconds. Never do
        // this on the foreground thread or repeat it during a scrolling frame.
        let text_system = cx.text_system().clone();
        let loading = cx.background_executor().spawn(async move {
            let started = std::time::Instant::now();
            let names = text_system.all_font_names();
            tracing::debug!(target: "beacon_ui::settings_perf", elapsed_us = started.elapsed().as_micros() as u64, families = names.len(), "settings font cache loaded");
            names
        });
        fonts._load = Some(cx.spawn(async move |this, cx| {
            let names = loading.await;
            let _ = this.update(cx, |fonts, cx| {
                fonts.names = Some(names.into());
                cx.notify();
            });
        }));
    });
    fonts
}
pub(crate) fn init(cx: &mut App) {
    cx.set_global(Windows::default());
}
pub(crate) fn application(cx: &mut App) {
    open(None, cx);
}
pub(crate) fn cluster(id: ClusterId, cx: &mut App) {
    open(Some(id), cx);
}
fn open(id: Option<ClusterId>, cx: &mut App) {
    cx.defer(move |cx| {
        let handle = match &id {
            Some(id) => cx.global::<Windows>().clusters.get(id.as_str()).copied(),
            None => cx.global::<Windows>().application,
        };
        if let Some(handle) = handle
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(880.), px(760.)),
                cx,
            ))),
            window_min_size: Some(size(px(650.), px(460.))),
            ..TitleBar::window_options()
        };
        let target = id.clone();
        match cx.open_window(options, move |window, cx| {
            let view = cx.new(|cx| PreferencesView::new(target, window, cx));
            window.set_window_title(&view.read(cx).title());
            window.activate_window();
            cx.new(|cx| Root::new(view, window, cx))
        }) {
            Ok(handle) => match id {
                Some(id) => {
                    cx.global_mut::<Windows>()
                        .clusters
                        .insert(id.to_string(), handle);
                }
                None => cx.global_mut::<Windows>().application = Some(handle),
            },
            Err(error) => tracing::error!(%error,"could not open preferences"),
        }
    });
}
fn input(
    label: &str,
    value: impl Into<SharedString>,
    masked: bool,
    window: &mut Window,
    cx: &mut Context<PreferencesView>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(label.to_string())
            .default_value(value)
            .masked(masked)
    })
}
fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}
fn field(label: &str, input: &Entity<InputState>) -> AnyElement {
    v_flex()
        .gap_1()
        .w_full()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(label.to_string()),
        )
        .child(Input::new(input))
        .into_any_element()
}
fn heading(text: &str) -> AnyElement {
    div()
        .mt_3()
        .mb_1()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.to_string())
        .into_any_element()
}
fn hint(text: &str, cx: &App) -> AnyElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
        .into_any_element()
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProxyChoice {
    Inherit,
    System,
    Direct,
    Custom,
}
struct ProxyEditor {
    choice: ProxyChoice,
    url: Entity<InputState>,
}
impl ProxyEditor {
    fn new(proxy: Option<Proxy>, window: &mut Window, cx: &mut Context<PreferencesView>) -> Self {
        let (choice, url) = match proxy {
            None => (ProxyChoice::Inherit, String::new()),
            Some(Proxy::System) => (ProxyChoice::System, String::new()),
            Some(Proxy::Direct) => (ProxyChoice::Direct, String::new()),
            Some(Proxy::Custom(url)) => (ProxyChoice::Custom, url),
        };
        Self {
            choice,
            url: input("Proxy URL", url, false, window, cx),
        }
    }
    fn read(&self, cx: &App) -> Result<Option<Proxy>, String> {
        let proxy = match self.choice {
            ProxyChoice::Inherit => None,
            ProxyChoice::System => Some(Proxy::System),
            ProxyChoice::Direct => Some(Proxy::Direct),
            ProxyChoice::Custom => Some(Proxy::Custom(value(&self.url, cx).trim().into())),
        };
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        Ok(proxy)
    }
}
struct ThemeEditor {
    appearance: Appearance,
    custom: CustomTheme,
    name: Entity<InputState>,
    font: Entity<InputState>,
    size: Entity<InputState>,
    mono: Entity<InputState>,
    mono_size: Entity<InputState>,
    role: String,
    color: Entity<InputState>,
}
struct ClusterEditor {
    id: ClusterId,
    alias: Entity<InputState>,
    icon: ClusterIcon,
    yaml_folding: crate::yaml_folding::YamlFolding,
    yaml_field: Entity<InputState>,
    metrics: u8,
    url: Entity<InputState>,
    token: Entity<InputState>,
    pod_cpu: Entity<InputState>,
    pod_memory: Entity<InputState>,
    node_cpu: Entity<InputState>,
    node_memory: Entity<InputState>,
}
enum Page {
    Application(ThemeEditor),
    Cluster(ClusterEditor),
}
struct PreferencesView {
    focus: FocusHandle,
    page: Page,
    proxy: ProxyEditor,
    message: Option<(bool, String)>,
    testing: bool,
    _test: Option<Task<()>>,
    fonts: Option<Entity<FontFamilies>>,
    _font_ready: Option<Subscription>,
    updates: beacon_updater::Preferences,
    _updates: Option<Subscription>,
}
impl PreferencesView {
    fn new(id: Option<ClusterId>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = settings::store(cx);
        let preferences = store.read(cx).preferences.clone();
        let message = store.read(cx).load_error.clone().map(|e| (true, e));
        let (page, proxy) = match id {
            None => {
                let custom = if let Appearance::Custom(name) = &preferences.appearance {
                    preferences.themes.get(name).cloned().unwrap_or_default()
                } else {
                    CustomTheme {
                        dark: preferences.appearance == Appearance::Dark,
                        ..Default::default()
                    }
                };
                let editor = ThemeEditor {
                    appearance: preferences.appearance.clone(),
                    name: input("Theme name", custom.name.clone(), false, window, cx),
                    font: input("Font family", custom.font_family.clone(), false, window, cx),
                    size: input("Font size", custom.font_size.to_string(), false, window, cx),
                    mono: input(
                        "Monospace font family",
                        custom.mono_font_family.clone(),
                        false,
                        window,
                        cx,
                    ),
                    mono_size: input(
                        "Monospace font size",
                        custom.mono_font_size.to_string(),
                        false,
                        window,
                        cx,
                    ),
                    role: "button.primary.foreground".into(),
                    color: input(
                        "Hex color",
                        custom
                            .colors
                            .get("button.primary.foreground")
                            .cloned()
                            .unwrap_or_default(),
                        false,
                        window,
                        cx,
                    ),
                    custom,
                };
                (
                    Page::Application(editor),
                    ProxyEditor::new(Some(preferences.proxy), window, cx),
                )
            }
            Some(id) => {
                let cluster = preferences
                    .clusters
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let (metrics, prom) = match cluster.metrics {
                    MetricsSource::Kubernetes => (0, Prometheus::default()),
                    MetricsSource::Prometheus(p) => (1, p),
                    MetricsSource::Disabled => (2, Prometheus::default()),
                };
                let editor = ClusterEditor {
                    id,
                    alias: input("Cluster alias", cluster.alias, false, window, cx),
                    icon: cluster.icon,
                    yaml_folding: cluster.yaml_folding,
                    yaml_field: input("e.g. spec.template.spec.containers", "", false, window, cx),
                    metrics,
                    url: input("Prometheus URL", prom.url, false, window, cx),
                    token: input(
                        "Bearer token (optional)",
                        prom.bearer_token,
                        true,
                        window,
                        cx,
                    ),
                    pod_cpu: input("Pod CPU query", prom.pod_cpu, false, window, cx),
                    pod_memory: input("Pod memory query", prom.pod_memory, false, window, cx),
                    node_cpu: input("Node CPU query", prom.node_cpu, false, window, cx),
                    node_memory: input("Node memory query", prom.node_memory, false, window, cx),
                };
                (
                    Page::Cluster(editor),
                    ProxyEditor::new(cluster.proxy, window, cx),
                )
            }
        };
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let fonts = matches!(&page, Page::Application(_)).then(|| font_families(cx));
        let font_ready = fonts
            .as_ref()
            .map(|fonts| cx.observe(fonts, |_, _, cx| cx.notify()));
        let updates = settings::store(cx).read(cx).preferences.updates.clone();
        let update_subscription =
            crate::updates::maybe_store(cx).map(|store| cx.observe(&store, |_, _, cx| cx.notify()));
        Self {
            updates,
            _updates: update_subscription,
            focus,
            page,
            proxy,
            message,
            testing: false,
            _test: None,
            fonts,
            _font_ready: font_ready,
        }
    }
    fn title(&self) -> String {
        match &self.page {
            Page::Application(_) => "Beacon — Settings".into(),
            Page::Cluster(c) => format!("Beacon — Cluster settings · {}", c.id.display_name()),
        }
    }
    fn sync_color(&mut self, cx: &App) -> Result<(), String> {
        if let Page::Application(t) = &mut self.page {
            let color = value(&t.color, cx).trim().to_string();
            if color.is_empty() {
                t.custom.colors.remove(&t.role);
            } else {
                let mut candidate = t.custom.clone();
                candidate.colors.insert(t.role.clone(), color);
                candidate.validate()?;
                t.custom = candidate;
            }
        }
        Ok(())
    }
    fn read_theme(&mut self, cx: &App) -> Result<Option<CustomTheme>, String> {
        if let Page::Application(t) = &self.page
            && matches!(t.appearance, Appearance::Custom(_))
        {
            self.sync_color(cx)?;
        }
        if let Page::Application(t) = &self.page
            && matches!(t.appearance, Appearance::Custom(_))
        {
            let mut custom = t.custom.clone();
            custom.name = value(&t.name, cx).trim().into();
            custom.font_family = value(&t.font, cx).trim().into();
            custom.font_size = value(&t.size, cx)
                .parse()
                .map_err(|_| "Invalid font size.")?;
            custom.mono_font_family = value(&t.mono, cx).trim().into();
            custom.mono_font_size = value(&t.mono_size, cx)
                .parse()
                .map_err(|_| "Invalid monospace font size.")?;
            custom.validate()?;
            let installed = self
                .fonts
                .as_ref()
                .and_then(|fonts| fonts.read(cx).names.as_ref())
                .ok_or("Fonts are loading. Try saving again in a moment.")?;
            for font in [&custom.font_family, &custom.mono_font_family] {
                if font != ".SystemUIFont" && !installed.iter().any(|f| f == font) {
                    return Err(format!(
                        "Font '{font}' is not installed. Choose a font from the menu."
                    ));
                }
            }
            Ok(Some(custom))
        } else {
            Ok(None)
        }
    }
    fn prometheus(c: &ClusterEditor, cx: &App) -> Prometheus {
        Prometheus {
            url: value(&c.url, cx).trim().into(),
            bearer_token: value(&c.token, cx).trim().into(),
            pod_cpu: value(&c.pod_cpu, cx),
            pod_memory: value(&c.pod_memory, cx),
            node_cpu: value(&c.node_cpu, cx),
            node_memory: value(&c.node_memory, cx),
        }
    }
    fn save(&mut self, reconnect: bool, window: &mut Window, cx: &mut Context<Self>) {
        let result = (|| {
            // Save also accepts the field currently being entered, so a draft
            // path is not silently discarded when the user clicks Save.
            if let Page::Cluster(c) = &self.page
                && !value(&c.yaml_field, cx).trim().is_empty()
            {
                self.add_yaml_field(window, cx)?;
            }
            let theme = self.read_theme(cx)?;
            let proxy = self.proxy.read(cx)?;
            let mut preferences = settings::store(cx).read(cx).preferences.clone();
            let mut target = None;
            match &self.page {
                Page::Application(t) => {
                    preferences.updates = self.updates.clone();
                    preferences.proxy = proxy.unwrap_or_default();
                    preferences.appearance = if let Some(theme) = theme {
                        let name = theme.name.clone();
                        preferences.themes.insert(name.clone(), theme);
                        Appearance::Custom(name)
                    } else {
                        t.appearance.clone()
                    };
                }
                Page::Cluster(c) => {
                    let alias = value(&c.alias, cx).trim().to_string();
                    if alias.len() > 120 {
                        return Err("Cluster alias must be at most 120 characters.".into());
                    }
                    let metrics = match c.metrics {
                        1 => MetricsSource::Prometheus(Self::prometheus(c, cx)),
                        2 => MetricsSource::Disabled,
                        _ => MetricsSource::Kubernetes,
                    };
                    preferences.clusters.insert(
                        c.id.to_string(),
                        ClusterSettings {
                            alias,
                            icon: c.icon.clone(),
                            proxy,
                            metrics,
                            yaml_folding: c.yaml_folding.clone(),
                        },
                    );
                    target = Some(c.id.clone());
                }
            }
            settings::save(preferences, window, cx)?;
            if reconnect && let Some(target) = target {
                settings::store(cx).update(cx, |_, cx| cx.emit(settings::Changed(Some(target))));
            }
            Ok::<_, String>(())
        })();
        self.message = Some(match result {
            Ok(()) => (
                false,
                if reconnect {
                    "Saved. Reconnecting cluster…"
                } else if matches!(self.page, Page::Cluster(_)) {
                    "Saved. YAML folding applies when resource YAML is next opened. No reconnect is needed for folding changes."
                } else {
                    "Saved. Connection changes apply when the cluster next connects."
                }
                .into(),
            ),
            Err(error) => (true, error),
        });
        cx.notify();
    }

    fn add_yaml_field(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if let Page::Cluster(c) = &mut self.page {
            c.yaml_folding.add(&value(&c.yaml_field, cx))?;
            c.yaml_field
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.message = None;
            cx.notify();
        }
        Ok(())
    }

    fn yaml_folding_form(&self, c: &ClusterEditor, cx: &mut Context<Self>) -> AnyElement {
        let rows = c
            .yaml_folding
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                h_flex()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .child(
                        Checkbox::new(("yaml-fold-field", index))
                            .label(field.path.clone())
                            .checked(field.collapsed)
                            .on_click(cx.listener(move |view, checked, _, cx| {
                                if let Page::Cluster(c) = &mut view.page
                                    && let Some(field) = c.yaml_folding.fields.get_mut(index)
                                {
                                    field.collapsed = *checked;
                                }
                                view.message = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("remove-yaml-fold-field", index))
                            .ghost()
                            .label("Remove")
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if let Page::Cluster(c) = &mut view.page
                                    && index < c.yaml_folding.fields.len()
                                {
                                    c.yaml_folding.fields.remove(index);
                                }
                                view.message = None;
                                cx.notify();
                            })),
                    )
            });
        v_flex().gap_2()
            .child(heading("YAML folding"))
            .child(hint("Checked fields collapse when resource YAML opens in this cluster. Uncheck all fields to keep YAML expanded. Changes apply on the next opening, without reconnecting.", cx))
            .children(rows)
            .child(h_flex().gap_2().items_center()
                .child(div().flex_1().min_w_0().child(Input::new(&c.yaml_field)))
                .child(Button::new("add-yaml-fold-field").outline().label("Add field")
                    .on_click(cx.listener(|view, _, window, cx| {
                        if let Err(error) = view.add_yaml_field(window, cx) {
                            view.message = Some((true, error));
                            cx.notify();
                        }
                    }))))
            .child(hint("Use dot-separated paths, such as metadata.annotations or spec.template.spec.containers. Paths below a list apply to every item.", cx))
            .child(Button::new("reset-yaml-folding").ghost().label("Restore defaults")
                .on_click(cx.listener(|view, _, window, cx| {
                    if let Page::Cluster(c) = &mut view.page {
                        c.yaml_folding = Default::default();
                        c.yaml_field.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    view.message = None;
                    cx.notify();
                })))
            .into_any_element()
    }
    fn select_theme(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let custom = settings::store(cx)
            .read(cx)
            .preferences
            .themes
            .get(&name)
            .cloned();
        if let (Some(custom), Page::Application(t)) = (custom, &mut self.page) {
            t.appearance = Appearance::Custom(name);
            t.name
                .update(cx, |s, cx| s.set_value(custom.name.clone(), window, cx));
            t.font.update(cx, |s, cx| {
                s.set_value(custom.font_family.clone(), window, cx)
            });
            t.size.update(cx, |s, cx| {
                s.set_value(custom.font_size.to_string(), window, cx)
            });
            t.mono.update(cx, |s, cx| {
                s.set_value(custom.mono_font_family.clone(), window, cx)
            });
            t.mono_size.update(cx, |s, cx| {
                s.set_value(custom.mono_font_size.to_string(), window, cx)
            });
            t.color.update(cx, |s, cx| {
                s.set_value(
                    custom.colors.get(&t.role).cloned().unwrap_or_default(),
                    window,
                    cx,
                )
            });
            t.custom = custom;
            self.message = None;
            cx.notify();
        }
    }
    fn choose_icon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choosing = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose an SVG cluster icon".into()),
        });
        let directory = settings::store(cx).read(cx).directory.clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = choosing.await
                && let Some(path) = paths.first()
            {
                let path = path.clone();
                let imported = cx
                    .background_executor()
                    .spawn(async move { import_icon(path, directory) })
                    .await;
                let _ = this.update(cx, |view, cx| {
                    match imported {
                        Ok(path) => {
                            if let Page::Cluster(c) = &mut view.page {
                                c.icon = ClusterIcon::Custom(path);
                            }
                            view.message = None;
                        }
                        Err(error) => view.message = Some((true, error)),
                    };
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn test_metrics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Cluster(c) = &self.page else {
            return;
        };
        let config = Self::prometheus(c, cx);
        let id = c.id.clone();
        let proxy = match self.proxy.read(cx) {
            Ok(proxy) => {
                proxy.unwrap_or_else(|| settings::store(cx).read(cx).preferences.proxy.clone())
            }
            Err(error) => {
                self.message = Some((true, error));
                cx.notify();
                return;
            }
        };
        if let Err(error) = config.validate() {
            self.message = Some((true, error));
            cx.notify();
            return;
        }
        self.testing = true;
        self.message = None;
        cx.notify();
        let reading = crate::bridge::Bridge::global(cx)
            .run(async move { beacon_kube::metrics::fetch_prometheus(&proxy, &config).await });
        self._test = Some(cx.spawn_in(window, async move |this, cx| {
            let result = reading.await;
            let _ = this.update(cx, |view, cx| {
                view.testing = false;
                view.message = Some(match result {
                    Ok(Ok(metrics)) => (
                        false,
                        format!(
                            "Prometheus is reachable: {} resource samples for {}.",
                            metrics.len(),
                            id.display_name()
                        ),
                    ),
                    Ok(Err(error)) => (true, error),
                    Err(_) => (true, "Metrics test was interrupted.".into()),
                });
                cx.notify();
            });
        }));
    }
    fn proxy_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let choices = [
            (ProxyChoice::Inherit, "Use global proxy"),
            (ProxyChoice::System, "Kubeconfig / environment"),
            (ProxyChoice::Direct, "Direct"),
            (ProxyChoice::Custom, "Custom proxy"),
        ];
        let cluster = matches!(self.page, Page::Cluster(_));
        let buttons = choices
            .into_iter()
            .filter(|(choice, _)| cluster || *choice != ProxyChoice::Inherit)
            .map(|(choice, label)| {
                Button::new(label)
                    .outline()
                    .selected(self.proxy.choice == choice)
                    .label(label)
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.proxy.choice = choice;
                        view.message = None;
                        cx.notify();
                    }))
            });
        v_flex().gap_2()
            .child(heading(if cluster { "Connection proxy" } else { "Global proxy" }))
            .child(h_flex().gap_2().flex_wrap().children(buttons))
            .when(self.proxy.choice == ProxyChoice::Custom, |form| {
                form.child(field("Proxy URL (http / https / socks5)", &self.proxy.url))
                    .when(value(&self.proxy.url, cx).starts_with("socks5:"), |form| {
                        form.child(hint("SOCKS5 cannot carry kubectl's SPDY Shell/Exec fallback. Use HTTP or HTTPS when that compatibility path is needed.", cx))
                    })
            })
            .child(hint("An explicit proxy overrides NO_PROXY. Direct bypasses both kubeconfig and environment proxies.", cx))
            .into_any_element()
    }
    fn save_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut preferences = settings::store(cx).read(cx).preferences.clone();
        preferences.updates = self.updates.clone();
        if let Err(error) = settings::save(preferences, window, cx) {
            self.updates = settings::store(cx).read(cx).preferences.updates.clone();
            self.message = Some((true, error));
        }
        cx.notify();
    }
    fn updates_form(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::updates::Status;
        use beacon_updater::Channel;
        let Some(store) = crate::updates::maybe_store(cx) else {
            return div().into_any_element();
        };
        let updater = store.read(cx);
        let can_install = updater.installation.can_install();
        let (message, release) = match &updater.status {
            Status::Idle => ("Updates have not been checked yet.".into(), None),
            Status::Checking => ("Checking for updates…".into(), None),
            Status::Current => ("You are up to date.".into(), None),
            Status::Available(r) => (
                format!(
                    "Beacon {} is available · {:.1} MB",
                    r.version,
                    r.asset.size as f64 / 1_000_000.
                ),
                Some(r),
            ),
            Status::Downloading {
                release,
                received,
                total,
            } => (
                format!(
                    "Downloading Beacon {} · {:.1} / {:.1} MB · {}%",
                    release.version,
                    *received as f64 / 1_000_000.,
                    *total as f64 / 1_000_000.,
                    received.saturating_mul(100) / (*total).max(1)
                ),
                Some(release),
            ),
            Status::Ready(download) => (
                format!(
                    "Beacon {} is ready to install. Signature verified.",
                    download.release.version
                ),
                Some(&download.release),
            ),
            Status::Installing => ("Preparing update and restart…".into(), None),
            Status::Failed { message, release } => (message.clone(), release.as_ref()),
        };
        let busy = matches!(
            updater.status,
            Status::Checking | Status::Downloading { .. } | Status::Installing
        );
        let ready = matches!(updater.status, Status::Ready(_));
        let download = matches!(
            updater.status,
            Status::Available(_)
                | Status::Failed {
                    release: Some(_),
                    ..
                }
        );
        let mut form = v_flex().gap_3()
            .child(heading("Updates"))
            .child(hint(&format!("Current version: {}", env!("CARGO_PKG_VERSION")), cx))
            .child(Checkbox::new("auto-check-updates").label("Automatically check for updates").checked(self.updates.auto_check)
                .on_click(cx.listener(|view, checked, window, cx| { view.updates.auto_check = *checked; view.save_updates(window, cx); })))
            .child(Checkbox::new("auto-download-updates").label("Automatically download updates").checked(self.updates.auto_download).disabled(!can_install)
                .on_click(cx.listener(|view, checked, window, cx| { view.updates.auto_download = *checked; view.save_updates(window, cx); })))
            .child(hint("Checks run shortly after startup and every 24 hours. Installation always requires restart confirmation. Downloads use the saved global proxy.", cx))
            .child(h_flex().gap_2().children([(Channel::Stable, "Stable"), (Channel::Development, "Development")].into_iter().map(|(channel, label)| {
                Button::new(label).outline().selected(self.updates.channel == channel).label(label).disabled(matches!(updater.status, Status::Installing))
                    .on_click(cx.listener(move |view, _, window, cx| { view.updates.channel = channel; view.save_updates(window, cx); }))
            })))
            .when(self.updates.channel == Channel::Development, |form| form.child(hint("Development releases may contain unfinished changes. Beacon will never automatically downgrade to an older version.", cx)))
            .child(div().text_sm().text_color(if matches!(updater.status, Status::Failed { .. }) { cx.theme().danger } else { cx.theme().foreground })
                .child(crate::copyable_text::copyable_text("update-status", message)))
            .when_some(updater.receipt.clone(), |form, receipt| form.child(crate::copyable_text::copyable_text("update-install-result", receipt)))
            .child(h_flex().gap_2().flex_wrap()
                .child(Button::new("check-updates").outline().label("Check for updates").disabled(busy || ready)
                    .on_click(|_, _, cx| crate::updates::store(cx).update(cx, |view, cx| view.check(cx))))
                .when(busy && !matches!(updater.status, Status::Installing), |bar| bar.child(Button::new("cancel-update").ghost().label("Cancel")
                    .on_click(|_, _, cx| crate::updates::store(cx).update(cx, |view, cx| view.cancel(cx)))))
                .when(download && can_install, |bar| bar.child(Button::new("download-update").primary().label(if matches!(updater.status, Status::Failed { .. }) { "Retry download" } else { "Download update" })
                    .on_click(|_, _, cx| crate::updates::store(cx).update(cx, |view, cx| view.download(cx)))))
                .when(ready, |bar| bar.child(Button::new("install-update").primary().label("Restart and install")
                    .on_click(|_, window, cx| crate::updates::store(cx).update(cx, |view, cx| view.restart(window, cx)))))
                .when_some(release.map(|r| r.page.to_string()), |bar, url| bar.child(Button::new("update-release-page").ghost().label("View release")
                    .on_click(move |_, _, cx| cx.open_url(&url)))));
        if let Status::Downloading {
            received, total, ..
        } = &updater.status
        {
            let fraction = (*received as f32 / (*total).max(1) as f32).clamp(0., 1.);
            form = form.child(
                div()
                    .w_full()
                    .h(px(4.))
                    .rounded_full()
                    .bg(cx.theme().muted)
                    .child(
                        div()
                            .h_full()
                            .w(relative(fraction))
                            .rounded_full()
                            .bg(cx.theme().primary),
                    ),
            );
        }
        if let Some(checked) = updater.last_checked {
            let seconds = checked.elapsed().map_or(0, |elapsed| elapsed.as_secs());
            form = form.child(hint(
                &if seconds < 60 {
                    "Last checked just now.".into()
                } else {
                    format!("Last checked {} minute(s) ago.", seconds / 60)
                },
                cx,
            ));
        }
        if let Some(release) = release {
            form = form.child(div().text_sm().child(crate::copyable_text::copyable_text(
                "update-release-notes",
                release.notes.chars().take(8192).collect::<String>(),
            )));
            if !can_install {
                let url = release.asset.url.to_string();
                form = form.child(
                    Button::new("get-update-package")
                        .primary()
                        .label("Get update package")
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            }
        }
        if !can_install {
            form = form.child(hint("This installation is managed externally. Download the package and install it with your package manager; automatic replacement is available for installed macOS/Windows apps and Linux AppImage.", cx));
        }
        form.into_any_element()
    }

    fn theme_form(&self, t: &ThemeEditor, cx: &mut Context<Self>) -> AnyElement {
        let names: Vec<_> = settings::store(cx)
            .read(cx)
            .preferences
            .themes
            .keys()
            .cloned()
            .collect();
        let weak = cx.entity().downgrade();
        let custom = matches!(t.appearance, Appearance::Custom(_));
        let appearances = [(false, "Light"), (true, "Dark")]
            .into_iter()
            .map(|(dark, label)| {
                let appearance = if dark {
                    Appearance::Dark
                } else {
                    Appearance::Light
                };
                Button::new(label)
                    .outline()
                    .selected(t.appearance == appearance)
                    .label(label)
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Page::Application(t) = &mut view.page {
                            t.appearance = appearance.clone();
                        }
                        view.message = None;
                        cx.notify();
                    }))
            });
        let saved = Button::new("saved-themes")
            .outline()
            .label("Saved themes")
            .dropdown_menu(move |mut menu, _, _| {
                for name in &names {
                    let target = name.clone();
                    let weak = weak.clone();
                    menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                        move |_, window, cx| {
                            let _ = weak.update(cx, |view, cx| {
                                view.select_theme(target.clone(), window, cx)
                            });
                        },
                    ));
                }
                menu
            });
        let form = v_flex().gap_3().child(heading("Appearance")).child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .children(appearances)
                .child(
                    Button::new("custom-theme")
                        .outline()
                        .selected(custom)
                        .label("Custom theme")
                        .on_click(cx.listener(|view, _, _, cx| {
                            if let Page::Application(t) = &mut view.page {
                                t.appearance = Appearance::Custom(t.custom.name.clone());
                            }
                            cx.notify();
                        })),
                )
                .child(saved),
        );
        if !custom {
            return form.into_any_element();
        }
        let bases = [(false, "Light base"), (true, "Dark base")]
            .into_iter()
            .map(|(dark, label)| {
                Button::new(label)
                    .outline()
                    .selected(t.custom.dark == dark)
                    .label(label)
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Page::Application(t) = &mut view.page {
                            t.custom.dark = dark;
                        }
                        cx.notify();
                    }))
            });
        let bases = h_flex().gap_2().children(bases);
        let roles = settings::color_roles();
        let weak = cx.entity().downgrade();
        let colors = Button::new("theme-color-role")
            .outline()
            .label(t.role.clone())
            .dropdown_menu(move |mut menu, _, _| {
                for role in roles {
                    let role = role.clone();
                    let weak = weak.clone();
                    menu = menu.item(PopupMenuItem::new(role.clone()).on_click(
                        move |_, window, cx| {
                            let _ = weak
                                .update(cx, |view, cx| view.select_color(role.clone(), window, cx));
                        },
                    ));
                }
                menu
            });
        form.child(field("Theme name (save a new name to create another theme)", &t.name))
            .child(bases)
            .child(h_flex().gap_3().child(field("Text font", &t.font)).child(field("Text size (px)", &t.size)))
            .child(h_flex().gap_3().child(field("Monospace font", &t.mono)).child(field("Monospace size (px)", &t.mono_size)))
            .child(self.font_picker(cx))
            .child(heading("Colors"))
            .child(hint("Choose any UI color token, including each button variant's text, background, hover and active colors. Empty values inherit the base theme.", cx))
            .child(colors)
            .child(field("Hex color (#RGB, #RRGGBB or #RRGGBBAA)", &t.color))
            .child(Button::new("set-theme-color").outline().label("Set color")
                .on_click(cx.listener(|view, _, _, cx| {
                    view.message = Some(match view.sync_color(cx) {
                        Ok(()) => (false, "Color added to this theme. Save to apply it.".into()),
                        Err(error) => (true, error),
                    });
                    cx.notify();
                }))
            )
            .child(v_flex().gap_1().children(t.custom.colors.iter().map(|(role, color)| {
                h_flex().gap_2().text_sm().child(role.clone()).child(color.clone())
            })))
            .into_any_element()
    }
    fn select_color(&mut self, role: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.sync_color(cx) {
            self.message = Some((true, error));
            cx.notify();
            return;
        }
        if let Page::Application(t) = &mut self.page {
            let color = t.custom.colors.get(&role).cloned().unwrap_or_default();
            t.role = role;
            t.color.update(cx, |s, cx| s.set_value(color, window, cx));
        }
        cx.notify();
    }
    fn font_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let fonts = self
            .fonts
            .as_ref()
            .and_then(|fonts| fonts.read(cx).names.clone());
        let loading = fonts.is_none();
        let weak = cx.entity().downgrade();
        let mono_weak = weak.clone();
        let mono_fonts = fonts.clone();
        h_flex()
            .gap_2()
            .child(
                Button::new("text-font-menu")
                    .ghost()
                    .disabled(loading)
                    .label(if loading {
                        "Loading fonts…"
                    } else {
                        "Choose text font"
                    })
                    .dropdown_menu(move |mut menu, _, _| {
                        for font in fonts.as_deref().unwrap_or_default() {
                            let font = font.clone();
                            let weak = weak.clone();
                            menu = menu.item(PopupMenuItem::new(font.clone()).on_click(
                                move |_, window, cx| {
                                    let _ = weak.update(cx, |view, cx| {
                                        if let Page::Application(t) = &mut view.page {
                                            t.font.update(cx, |s, cx| {
                                                s.set_value(font.clone(), window, cx)
                                            });
                                        }
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
            .child(
                Button::new("mono-font-menu")
                    .ghost()
                    .disabled(loading)
                    .label("Choose monospace font")
                    .dropdown_menu(move |mut menu, _, _| {
                        for font in mono_fonts.as_deref().unwrap_or_default() {
                            let font = font.clone();
                            let weak = mono_weak.clone();
                            menu = menu.item(PopupMenuItem::new(font.clone()).on_click(
                                move |_, window, cx| {
                                    let _ = weak.update(cx, |view, cx| {
                                        if let Page::Application(t) = &mut view.page {
                                            t.mono.update(cx, |s, cx| {
                                                s.set_value(font.clone(), window, cx)
                                            });
                                        }
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
            .into_any_element()
    }
    fn cluster_form(&self, c: &ClusterEditor, cx: &mut Context<Self>) -> AnyElement {
        let icons: Vec<_> = [
            (ClusterIcon::Kubernetes, "Kubernetes"),
            (ClusterIcon::Server, "Server"),
            (ClusterIcon::Cloud, "Cloud"),
            (ClusterIcon::Workloads, "Workloads"),
            (ClusterIcon::Storage, "Storage"),
            (ClusterIcon::Shield, "Shield"),
        ]
        .into_iter()
        .map(|(icon, label)| {
            Button::new(label)
                .outline()
                .selected(c.icon == icon)
                .icon(settings::icon(&icon))
                .label(label)
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Page::Cluster(c) = &mut view.page {
                        c.icon = icon.clone();
                    }
                    cx.notify();
                }))
        })
        .collect();
        let metrics: Vec<_> = [
            (0, "Kubernetes Metrics API"),
            (1, "Prometheus"),
            (2, "Disabled"),
        ]
        .into_iter()
        .map(|(source, label)| {
            Button::new(label)
                .outline()
                .selected(c.metrics == source)
                .label(label)
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Page::Cluster(c) = &mut view.page {
                        c.metrics = source;
                    }
                    view.message = None;
                    cx.notify();
                }))
        })
        .collect();
        v_flex().gap_3().child(heading("Cluster identity"))
            .child(hint(&format!("Kubeconfig context: {}", c.id), cx))
            .child(field("Alias (empty uses the kubeconfig name)", &c.alias))
            .child(h_flex().gap_2().flex_wrap().children(icons).child(
                Button::new("custom-cluster-icon").outline().label("Choose SVG…")
                    .on_click(cx.listener(|view, _, window, cx| view.choose_icon(window, cx)))
            ))
            .when(matches!(c.icon, ClusterIcon::Custom(_)), |form| {
                form.child(h_flex().gap_2().child(settings::icon(&c.icon)).child("Custom SVG icon"))
            })
            .child(self.yaml_folding_form(c, cx))
            .child(self.proxy_form(cx))
            .child(heading("Metrics source"))
            .child(h_flex().gap_2().children(metrics))
            .when(c.metrics == 1, |form| {
                form.child(field("Prometheus URL (base path supported)", &c.url))
                    .child(field("Bearer token (optional)", &c.token))
                    .child(hint("Queries return CPU in cores and memory in bytes. Pod results require namespace/pod labels; node results require a node label. Add cluster selectors when Prometheus contains multiple clusters.", cx))
                    .child(field("Pod CPU query", &c.pod_cpu))
                    .child(field("Pod memory query", &c.pod_memory))
                    .child(field("Node CPU query", &c.node_cpu))
                    .child(field("Node memory query", &c.node_memory))
                    .child(Button::new("test-metrics").outline().disabled(self.testing)
                        .label(if self.testing { "Testing…" } else { "Test metrics source" })
                        .on_click(cx.listener(|view, _, window, cx| view.test_metrics(window, cx)))
                    )
            })
            .when(c.metrics == 0, |form| {
                form.child(hint("Uses metrics.k8s.io. Clusters without metrics-server show no CPU or memory samples.", cx))
            })
            .into_any_element()
    }
}
impl Render for PreferencesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.page {
            Page::Application(t) => v_flex()
                .gap_4()
                .child(self.updates_form(cx))
                .child(self.theme_form(t, cx))
                .child(self.proxy_form(cx))
                .into_any_element(),
            Page::Cluster(c) => self.cluster_form(c, cx),
        };
        let directory = settings::store(cx).read(cx).directory.clone();
        let cluster = matches!(self.page, Page::Cluster(_));
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(|_: &crate::app::CloseTab, window, cx| {
                window.defer(cx, |window, _| window.remove_window())
            })
            .child(
                TitleBar::new().child(div().font_weight(FontWeight::SEMIBOLD).child(self.title())),
            )
            .child(
                div()
                    .id("preferences-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().p_4().gap_3().child(body).child(hint(
                        &format!("Saved locally in {}", directory.display()),
                        cx,
                    ))),
            )
            .when_some(self.message.clone(), |page, (error, message)| {
                page.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_sm()
                        .text_color(if error {
                            cx.theme().danger
                        } else {
                            cx.theme().success
                        })
                        .child(crate::copyable_text::copyable_text(
                            "preferences-message",
                            message,
                        )),
                )
            })
            .child(
                h_flex()
                    .justify_between()
                    .p_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("open-settings-folder")
                            .ghost()
                            .label("Open config folder")
                            .on_click(cx.listener(move |view, _, _, cx| {
                                match std::fs::create_dir_all(&directory) {
                                    Ok(()) => cx.open_with_system(&directory),
                                    Err(error) => {
                                        view.message = Some((true, error.to_string()));
                                        cx.notify();
                                    }
                                }
                            })),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("save-preferences")
                                    .primary()
                                    .label("Save")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.save(false, window, cx)
                                    })),
                            )
                            .when(cluster, |bar| {
                                bar.child(
                                    Button::new("save-reconnect")
                                        .outline()
                                        .label("Save and reconnect")
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.save(true, window, cx)
                                        })),
                                )
                            }),
                    ),
            )
    }
}
fn import_icon(path: PathBuf, directory: PathBuf) -> Result<PathBuf, String> {
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
    {
        return Err("Choose an SVG image.".into());
    }
    let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if metadata.len() > 512 * 1024 {
        return Err("SVG icons must be smaller than 512 KiB.".into());
    }
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = std::str::from_utf8(&data).map_err(|_| "SVG must contain UTF-8 text.")?;
    if !text.contains("<svg") {
        return Err("The file is not an SVG image.".into());
    }
    use sha2::{Digest, Sha256};
    let icons = directory.join("icons");
    std::fs::create_dir_all(&icons).map_err(|e| e.to_string())?;
    let path = icons.join(format!("{:x}.svg", Sha256::digest(&data)));
    std::fs::write(&path, data).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(all(test, feature = "ui-tests"))]
mod update_tests {
    use super::*;
    use crate::updates::Status;
    use beacon_updater::{Asset, Format, Release};
    use gpui_kit::test::TestWindowExt as _;

    #[::core::prelude::v1::test]
    fn settings_render_update_states_without_retained_views() {
        let directory = tempfile::tempdir().unwrap();
        let cx = &mut TestAppContext::single();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
            crate::app::init(directory.path().join("logs"), cx);
            settings::store(cx).update(cx, |state, _| {
                state.preferences = settings::Preferences::default();
                state.directory = directory.path().to_owned();
                state.load_error = None;
            });
            crate::Bridge::init(cx).unwrap();
            crate::updates::init(cx);
        });
        let (window, _) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| PreferencesView::new(None, window, cx))
            })
            .unwrap()
        });
        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| {
                window.draw(cx).clear(cx);
            })
            .unwrap();
        };
        draw(cx);
        cx.update_window(window, |_, window, _| {
            assert!(window.find("check-updates").visible());
            assert!(window.find("auto-check-updates").visible());
        })
        .unwrap();
        let release = Release {
            version: "0.2.8".parse().unwrap(),
            notes: "Release notes".into(),
            page: "https://github.com/yoogoc/beacon/releases/tag/v0.2.8"
                .parse()
                .unwrap(),
            asset: Asset {
                url: "https://github.com/yoogoc/beacon/releases/download/v0.2.8/Beacon.app.tar.gz"
                    .parse()
                    .unwrap(),
                signature: String::new(),
                size: 1024,
                format: Format::App,
            },
        };
        cx.update(|cx| {
            crate::updates::store(cx).update(cx, |updater, cx| {
                updater.installation = beacon_updater::Installation::MacApp {
                    bundle: "/tmp/Beacon.app".into(),
                    executable: "/tmp/Beacon.app/Contents/MacOS/beacon".into(),
                };
                updater.status = Status::Available(release.clone());
                cx.notify();
            })
        });
        draw(cx);
        cx.update_window(window, |_, window, _| {
            assert!(window.find("download-update").visible())
        })
        .unwrap();
        for received in [0, 512, 1024] {
            cx.update(|cx| {
                crate::updates::store(cx).update(cx, |updater, cx| {
                    updater.status = Status::Downloading {
                        release: release.clone(),
                        received,
                        total: 1024,
                    };
                    cx.notify();
                })
            });
            draw(cx);
            cx.update_window(window, |_, window, _| {
                assert!(window.find("cancel-update").visible())
            })
            .unwrap();
        }
        cx.update(|cx| {
            crate::updates::store(cx).update(cx, |updater, cx| {
                updater.status = Status::Failed {
                    message: "Signature verification failed".into(),
                    release: Some(release.clone()),
                };
                cx.notify();
            })
        });
        draw(cx);
        cx.update_window(window, |_, window, _| {
            assert!(window.find("download-update").visible())
        })
        .unwrap();
        cx.update(|cx| {
            crate::updates::store(cx).update(cx, |updater, cx| {
                updater.installation = beacon_updater::Installation::Managed;
                updater.status = Status::Available(release);
                cx.notify();
            })
        });
        draw(cx);
        cx.update_window(window, |_, window, _| {
            assert!(window.find("get-update-package").visible())
        })
        .unwrap();
    }
}
