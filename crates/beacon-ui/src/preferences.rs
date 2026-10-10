//! Application and cluster settings, presented in independent native windows.
use crate::settings::{self, Appearance, ClusterIcon, ClusterSettings, CustomTheme};
use beacon_kube::{
    ClusterId,
    connection::{MetricsSource, Prometheus, Proxy},
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _};
use gpui_kit::component::{
    Icon,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    radio::Radio,
    switch::Switch,
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
        .gap_2()
        .flex_1()
        .min_w_0()
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
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .child(text.to_string())
        .into_any_element()
}
fn setting_row(label: &str, description: &str, control: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_4()
        .py_4()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(label.to_owned()),
                )
                .child(hint(description, cx)),
        )
        .child(div().flex_shrink_0().child(control))
        .into_any_element()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Appearance,
    Themes,
    Network,
    Updates,
    Identity,
    Connection,
    Metrics,
    Yaml,
}
impl Section {
    const APPLICATION: [Self; 4] = [Self::Appearance, Self::Themes, Self::Network, Self::Updates];
    const CLUSTER: [Self; 4] = [Self::Identity, Self::Connection, Self::Metrics, Self::Yaml];

    fn id(self) -> &'static str {
        match self {
            Self::Appearance => "settings-appearance",
            Self::Themes => "settings-themes",
            Self::Network => "settings-network",
            Self::Updates => "settings-updates",
            Self::Identity => "settings-identity",
            Self::Connection => "settings-connection",
            Self::Metrics => "settings-metrics",
            Self::Yaml => "settings-yaml",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Themes => "Custom themes",
            Self::Network => "Network",
            Self::Updates => "Updates",
            Self::Identity => "Identity",
            Self::Connection => "Connection",
            Self::Metrics => "Metrics",
            Self::Yaml => "YAML editor",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::Identity => "Cluster identity",
            Self::Network => "Global proxy",
            Self::Metrics => "Metrics source",
            _ => self.label(),
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::Appearance => "Make Beacon feel at home.",
            Self::Themes => "Fine-tune typography and colors, with a live preview.",
            Self::Network => "Choose how Beacon connects to clusters and checks for updates.",
            Self::Updates => "Keep Beacon up to date on your preferred release channel.",
            Self::Identity => "Give this cluster a familiar name and icon.",
            Self::Connection => "Configure the proxy used when connecting to this cluster.",
            Self::Metrics => "Choose where pod and node usage metrics come from.",
            Self::Yaml => "Choose which fields start collapsed when opening a resource.",
        }
    }
    fn icon(self) -> IconName {
        match self {
            Self::Appearance => IconName::Palette,
            Self::Themes => IconName::Paintbrush,
            Self::Network => IconName::Network,
            Self::Updates => IconName::Download,
            Self::Identity => IconName::ShipWheel,
            Self::Connection => IconName::Unplug,
            Self::Metrics => IconName::ChartNoAxesCombined,
            Self::Yaml => IconName::FileText,
        }
    }
}
fn hint(text: &str, cx: &App) -> AnyElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
        .into_any_element()
}
fn inline_action(button: impl IntoElement) -> AnyElement {
    h_flex().child(button).into_any_element()
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
    light_preview: gpui_kit::component::Theme,
    dark_preview: gpui_kit::component::Theme,
    preview: gpui_kit::component::Theme,
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
    Application(Box<ThemeEditor>),
    Cluster(Box<ClusterEditor>),
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
    section: Section,
    queries_expanded: bool,
    dirty: bool,
    _inputs: Vec<Subscription>,
    _connections: Subscription,
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
                    light_preview: settings::preview_theme(&Appearance::Light, &custom, cx),
                    dark_preview: settings::preview_theme(&Appearance::Dark, &custom, cx),
                    preview: settings::preview_theme(&preferences.appearance, &custom, cx),
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
                    Page::Application(Box::new(editor)),
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
                    Page::Cluster(Box::new(editor)),
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
        let inputs = match &page {
            Page::Application(t) => {
                vec![&t.name, &t.font, &t.size, &t.mono, &t.mono_size, &t.color]
            }
            Page::Cluster(c) => vec![
                &c.alias,
                &c.yaml_field,
                &c.url,
                &c.token,
                &c.pod_cpu,
                &c.pod_memory,
                &c.node_cpu,
                &c.node_memory,
            ],
        };
        let input_subscriptions = inputs
            .into_iter()
            .chain([&proxy.url])
            .map(|input| {
                cx.subscribe(input, |view, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        if view.section == Section::Themes
                            && let Page::Application(t) = &mut view.page
                        {
                            t.appearance = Appearance::Custom(t.custom.name.clone());
                        }
                        view.dirty = true;
                        view.message = None;
                        view.refresh_theme_preview(cx);
                        cx.notify();
                    }
                })
            })
            .collect();
        let section = if matches!(page, Page::Application(_)) {
            Section::Appearance
        } else {
            Section::Identity
        };
        let connections = cx
            .global::<crate::connections::SharedConnections>()
            .0
            .clone();
        let connection_subscription = cx.observe(&connections, |_, _, cx| cx.notify());
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
            section,
            queries_expanded: false,
            dirty: false,
            _inputs: input_subscriptions,
            _connections: connection_subscription,
        }
    }
    fn title(&self) -> String {
        match &self.page {
            Page::Application(_) => "Beacon — Settings".into(),
            Page::Cluster(c) => format!("Beacon — Cluster settings · {}", c.id.display_name()),
        }
    }
    fn refresh_theme_preview(&mut self, cx: &App) {
        let Page::Application(t) = &mut self.page else {
            return;
        };
        let mut custom = t.custom.clone();
        custom.font_family = value(&t.font, cx);
        custom.mono_font_family = value(&t.mono, cx);
        if let Ok(size) = value(&t.size, cx).parse::<f32>()
            && (10. ..=30.).contains(&size)
        {
            custom.font_size = size;
        }
        if let Ok(size) = value(&t.mono_size, cx).parse::<f32>()
            && (10. ..=30.).contains(&size)
        {
            custom.mono_font_size = size;
        }
        let color = value(&t.color, cx).trim().to_owned();
        if color.is_empty() {
            custom.colors.remove(&t.role);
        } else if gpui_kit::component::try_parse_color(&color).is_ok() {
            custom.colors.insert(t.role.clone(), color);
        }
        let appearance = if self.section == Section::Themes {
            Appearance::Custom(custom.name.clone())
        } else {
            t.appearance.clone()
        };
        t.preview = settings::preview_theme(&appearance, &custom, cx);
    }
    fn select_section(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        self.section = section;
        self.refresh_theme_preview(cx);
        cx.notify();
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
            self.dirty = false;
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
                    "Saved. YAML folding applies the next time a resource opens. Reconnect to apply connection changes."
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
            self.dirty = true;
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
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_sm()
                            .child(crate::copyable_text::copyable_text(
                                ("yaml-fold-path", index),
                                field.path.clone(),
                            )),
                    )
                    .child(
                        Switch::new(("yaml-fold-field", index))
                            .accessibility_label(format!("Collapse {}", field.path))
                            .color(cx.theme().link)
                            .checked(field.collapsed)
                            .on_click(cx.listener(move |view, checked, _, cx| {
                                if let Page::Cluster(c) = &mut view.page
                                    && let Some(field) = c.yaml_folding.fields.get_mut(index)
                                {
                                    field.collapsed = *checked;
                                }
                                view.dirty = true;
                                view.message = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("remove-yaml-fold-field", index))
                            .ghost()
                            .icon(IconName::X)
                            .tooltip(format!("Remove {}", field.path))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if let Page::Cluster(c) = &mut view.page
                                    && index < c.yaml_folding.fields.len()
                                {
                                    c.yaml_folding.fields.remove(index);
                                }
                                view.dirty = true;
                                view.message = None;
                                cx.notify();
                            })),
                    )
            });
        v_flex().gap_3()
            .child(heading("Collapsed by default"))
            .child(h_flex().text_xs().text_color(cx.theme().muted_foreground).justify_between()
                .pb_2().border_b_1().border_color(cx.theme().border)
                .child("Field path").child(div().pr(px(46.)).child("Collapse")))
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
            .child(inline_action(Button::new("reset-yaml-folding").link().icon(IconName::RotateCcw).label("Restore defaults")
                .on_click(cx.listener(|view, _, window, cx| {
                    if let Page::Cluster(c) = &mut view.page {
                        c.yaml_folding = Default::default();
                        c.yaml_field.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    view.dirty = true;
                    view.message = None;
                    cx.notify();
                }))))
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
            self.dirty = true;
            self.refresh_theme_preview(cx);
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
                            view.dirty = true;
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
        let cluster = matches!(self.page, Page::Cluster(_));
        let choices = [
            (
                ProxyChoice::Inherit,
                "Use global proxy",
                "Follow the application network settings.",
            ),
            (
                ProxyChoice::System,
                if cluster {
                    "Kubeconfig / environment"
                } else {
                    "System proxy"
                },
                if cluster {
                    "Use your existing network configuration."
                } else {
                    "Use system proxy settings for updates, and kubeconfig / environment settings for clusters."
                },
            ),
            (
                ProxyChoice::Direct,
                "Direct connection",
                "Connect without a proxy.",
            ),
            (
                ProxyChoice::Custom,
                "Custom proxy",
                "Set an explicit HTTP, HTTPS or SOCKS5 proxy.",
            ),
        ];
        let buttons = choices
            .into_iter()
            .filter(|(choice, _, _)| cluster || *choice != ProxyChoice::Inherit)
            .enumerate()
            .map(|(index, (choice, label, description))| {
                Radio::new(("proxy-choice", index))
                    .w_full()
                    .p_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .checked(self.proxy.choice == choice)
                    .label(label)
                    .child(hint(description, cx))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.proxy.choice = choice;
                        view.dirty = true;
                        view.message = None;
                        cx.notify();
                    }))
            });
        v_flex().gap_4()
            .child(v_flex().w_full().border_1().border_color(cx.theme().border).rounded_lg().overflow_hidden().children(buttons))
            .when(self.proxy.choice == ProxyChoice::Inherit, |form| {
                let global = settings::store(cx).read(cx).preferences.proxy.clone();
                let label = match global {
                    Proxy::System => "System proxy / kubeconfig / environment".to_owned(),
                    Proxy::Direct => "Direct connection".to_owned(),
                    Proxy::Custom(url) => url,
                };
                form.child(v_flex().p_3().gap_1().rounded_lg().bg(cx.theme().muted)
                    .child(hint("Global proxy", cx))
                    .child(crate::copyable_text::copyable_text("inherited-proxy", label)))
            })
            .when(self.proxy.choice == ProxyChoice::Custom, |form| {
                form.child(field("Proxy URL (http / https / socks5)", &self.proxy.url))
                    .when(value(&self.proxy.url, cx).starts_with("socks5:"), |form| {
                        form.child(hint("SOCKS5 cannot carry kubectl's SPDY Shell/Exec fallback. Use HTTP or HTTPS when that compatibility path is needed.", cx))
                    })
            })
            .child(hint(if cluster { "An explicit proxy overrides NO_PROXY. Direct bypasses both kubeconfig and environment proxies." } else { "An explicit proxy overrides NO_PROXY. Direct bypasses system, kubeconfig and environment proxies." }, cx))
            .child(hint(if cluster { "Connection changes take effect after reconnecting." } else { "Clusters can override this setting in their connection preferences." }, cx))
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
        let check_button = Button::new("check-updates")
            .outline()
            .icon(IconName::RefreshCw)
            .label("Check for updates")
            .disabled(busy || ready)
            .on_click(|_, _, cx| crate::updates::store(cx).update(cx, |view, cx| view.check(cx)));
        let mut form = v_flex().gap_3()
            .child(h_flex().gap_3().p_4().rounded_lg().bg(cx.theme().muted).items_center()
                .child(Icon::new(IconName::RadioTower).size_6().text_color(cx.theme().link))
                .child(v_flex().flex_1().gap_1()
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("Beacon {}", env!("CARGO_PKG_VERSION"))))
                    .child(hint(if self.updates.channel == Channel::Stable { "Stable release" } else { "Development release" }, cx)))
                .child(check_button))
            .child(setting_row("Check automatically", "Check for new releases in the background.",
                Switch::new("auto-check-updates").accessibility_label("Automatically check for updates").color(cx.theme().link).checked(self.updates.auto_check)
                    .on_click(cx.listener(|view, checked, window, cx| { view.updates.auto_check = *checked; view.save_updates(window, cx); })), cx))
            .child(setting_row("Download automatically", "Ask before restarting to install an update.",
                Switch::new("auto-download-updates").accessibility_label("Automatically download updates").color(cx.theme().link).checked(self.updates.auto_download).disabled(!can_install)
                    .on_click(cx.listener(|view, checked, window, cx| { view.updates.auto_download = *checked; view.save_updates(window, cx); })), cx))
            .child(hint("Checks run once at startup and every 24 hours when enabled. Checks and downloads use the system proxy by default, or the saved global proxy override. Installation always requires restart confirmation.", cx))
            .child(setting_row("Release channel", "Choose which releases you receive.", h_flex().gap_2().children([(Channel::Stable, "Stable"), (Channel::Development, "Development")].into_iter().map(|(channel, label)| {
                Button::new(label).outline().small().selected(self.updates.channel == channel).label(label).disabled(matches!(updater.status, Status::Installing))
                    .on_click(cx.listener(move |view, _, window, cx| { view.updates.channel = channel; view.save_updates(window, cx); }))
            })), cx))
            .when(self.updates.channel == Channel::Development, |form| form.child(hint("Development releases may contain unfinished changes. Beacon will never automatically downgrade to an older version.", cx)))
            .child(div().text_sm().text_color(if matches!(updater.status, Status::Failed { .. }) { cx.theme().danger } else { cx.theme().foreground })
                .child(crate::copyable_text::copyable_text("update-status", message)))
            .when_some(updater.receipt.clone(), |form, receipt| form.child(crate::copyable_text::copyable_text("update-install-result", receipt)))
            .child(h_flex().gap_2().flex_wrap()
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

    fn saved_theme_picker(&self, t: &ThemeEditor, cx: &mut Context<Self>) -> AnyElement {
        let names: Vec<_> = settings::store(cx)
            .read(cx)
            .preferences
            .themes
            .keys()
            .cloned()
            .collect();
        let empty = names.is_empty();
        let weak = cx.entity().downgrade();
        Button::new("saved-themes")
            .outline()
            .w(px(200.))
            .disabled(empty)
            .label(if empty {
                "No saved themes".to_owned()
            } else if let Appearance::Custom(name) = &t.appearance {
                name.clone()
            } else {
                "Choose a theme".to_owned()
            })
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
            })
            .into_any_element()
    }

    fn theme_thumbnail(theme: &gpui_kit::component::Theme) -> AnyElement {
        h_flex()
            .w_full()
            .h(px(78.))
            .rounded_md()
            .overflow_hidden()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                v_flex()
                    .w(relative(0.25))
                    .h_full()
                    .p_2()
                    .gap_2()
                    .bg(theme.muted)
                    .children((0..3).map(|_| {
                        div()
                            .w_full()
                            .h(px(3.))
                            .rounded_full()
                            .bg(theme.muted_foreground.opacity(0.45))
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .p_2()
                    .gap_2()
                    .child(
                        div()
                            .w(relative(0.6))
                            .h(px(4.))
                            .rounded_full()
                            .bg(theme.link),
                    )
                    .children([1., 1., 0.7].into_iter().map(|width| {
                        div()
                            .w(relative(width))
                            .h(px(4.))
                            .rounded_full()
                            .bg(theme.muted_foreground.opacity(0.35))
                    })),
            )
            .into_any_element()
    }

    fn appearance_form(&self, t: &ThemeEditor, cx: &mut Context<Self>) -> AnyElement {
        let choices = [
            (Appearance::Light, "Light", &t.light_preview),
            (Appearance::Dark, "Dark", &t.dark_preview),
            (
                Appearance::Custom(t.custom.name.clone()),
                "Custom",
                &t.preview,
            ),
        ]
        .into_iter()
        .map(|(appearance, label, theme)| {
            let selected = match &appearance {
                Appearance::Custom(_) => matches!(t.appearance, Appearance::Custom(_)),
                _ => t.appearance == appearance,
            };
            Button::new(label)
                .outline()
                .selected(selected)
                .accessibility_label(format!("{label} theme"))
                .flex_1()
                .min_w_0()
                .h_auto()
                .p_2()
                .rounded_lg()
                .bg(if selected {
                    cx.theme().link.opacity(0.09)
                } else {
                    cx.theme().background
                })
                .border_color(if selected {
                    cx.theme().link
                } else {
                    cx.theme().border
                })
                .text_color(cx.theme().foreground)
                .child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(Self::theme_thumbnail(theme))
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .text_sm()
                                .child(label)
                                .child(Icon::new(IconName::CircleCheck).size_4().text_color(
                                    if selected {
                                        cx.theme().link
                                    } else {
                                        cx.theme().transparent
                                    },
                                )),
                        ),
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Page::Application(t) = &mut view.page {
                        t.appearance = appearance.clone();
                    }
                    view.dirty = true;
                    view.message = None;
                    view.refresh_theme_preview(cx);
                    cx.notify();
                }))
        });
        v_flex()
            .gap_3()
            .w_full()
            .child(heading("Theme"))
            .child(h_flex().w_full().gap_3().items_stretch().children(choices))
            .child(setting_row(
                "Saved custom theme",
                "Stored locally on this device.",
                self.saved_theme_picker(t, cx),
                cx,
            ))
            .child(inline_action(
                Button::new("edit-custom-theme")
                    .link()
                    .icon(IconName::SlidersHorizontal)
                    .label("Edit custom theme")
                    .on_click(cx.listener(|view, _, window, cx| {
                        if let Page::Application(t) = &mut view.page {
                            t.appearance = Appearance::Custom(t.custom.name.clone());
                        }
                        view.dirty = true;
                        view.select_section(Section::Themes, window, cx);
                    })),
            ))
            .child(hint(
                "Theme changes apply across Beacon windows after saving.",
                cx,
            ))
            .child(heading("Preview"))
            .child(self.theme_preview(t))
            .into_any_element()
    }

    fn theme_preview(&self, t: &ThemeEditor) -> AnyElement {
        let theme = &t.preview;
        v_flex()
            .id("theme-preview")
            .w_full()
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .text_size(theme.font_size * 0.82)
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(11.))
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::RadioTower).size_3())
                    .child("Beacon · Theme preview"),
            )
            .child(
                h_flex()
                    .items_stretch()
                    .child(
                        v_flex()
                            .w(px(90.))
                            .flex_shrink_0()
                            .bg(theme.muted)
                            .p_3()
                            .gap_3()
                            .text_size(px(11.))
                            .child(div().text_color(theme.link).child("Workloads"))
                            .child(div().text_color(theme.muted_foreground).child("Network"))
                            .child(div().text_color(theme.muted_foreground).child("Config"))
                            .child(div().text_color(theme.muted_foreground).child("Storage")),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .p_3()
                            .gap_3()
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div().font_weight(FontWeight::MEDIUM).child("Pods"),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(theme.muted_foreground)
                                                    .font_family(theme.mono_font_family.clone())
                                                    .child("default"),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .rounded_md()
                                            .px_2()
                                            .py_1()
                                            .text_size(px(11.))
                                            .bg(theme.button_primary)
                                            .text_color(theme.button_primary_foreground)
                                            .child("Create"),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .pb_2()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .text_size(px(11.))
                                    .text_color(theme.muted_foreground)
                                    .child("Name")
                                    .child("Status"),
                            )
                            .children(["api-server", "worker"].into_iter().map(|name| {
                                h_flex()
                                    .justify_between()
                                    .gap_2()
                                    .text_size(px(11.))
                                    .child(name)
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .items_center()
                                            .text_color(theme.success)
                                            .child(
                                                div().size(px(5.)).rounded_full().bg(theme.success),
                                            )
                                            .child("Running"),
                                    )
                            })),
                    ),
            )
            .into_any_element()
    }

    fn theme_form(&self, t: &ThemeEditor, cx: &mut Context<Self>) -> AnyElement {
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
                            t.appearance = Appearance::Custom(t.custom.name.clone());
                        }
                        view.dirty = true;
                        view.refresh_theme_preview(cx);
                        cx.notify();
                    }))
            });
        let roles = settings::color_roles();
        let weak = cx.entity().downgrade();
        let colors = Button::new("theme-color-role")
            .outline()
            .w_full()
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
        let swatch =
            gpui_kit::component::try_parse_color(&value(&t.color, cx)).unwrap_or(cx.theme().muted);
        v_flex().gap_5().w_full()
            .child(field("Theme name", &t.name))
            .child(hint("Save with a new name to create another theme. Themes are stored locally.", cx))
            .child(setting_row("Base theme", "Use the base colors for any unset fields.", h_flex().gap_2().children(bases), cx))
            .child(v_flex().gap_3().child(heading("Typography"))
                .child(h_flex().gap_3().items_end().child(field("Interface font", &t.font))
                    .child(div().w(px(100.)).flex_shrink_0().child(field("Size · px", &t.size))))
                .child(h_flex().gap_3().items_end().child(field("Monospace font", &t.mono))
                    .child(div().w(px(100.)).flex_shrink_0().child(field("Size · px", &t.mono_size))))
                .child(self.font_picker(cx)))
            .child(v_flex().gap_3().child(heading("Colors"))
                .child(hint("Choose any UI color, including button text, background, hover and active states. Empty values inherit the base theme.", cx))
                .child(colors)
                .child(h_flex().gap_3().items_end()
                    .child(field("Hex color · #RGB, #RRGGBB or #RRGGBBAA", &t.color))
                    .child(div().size(px(32.)).flex_shrink_0().rounded_md().border_1().border_color(cx.theme().border).bg(swatch))
                    .child(Button::new("set-theme-color").outline().label("Set color")
                        .on_click(cx.listener(|view, _, _, cx| {
                            if let Page::Application(t) = &mut view.page { t.appearance = Appearance::Custom(t.custom.name.clone()); }
                            view.message = Some(match view.sync_color(cx) {
                                Ok(()) => { view.dirty = true; (false, "Color added to this theme. Save to apply it.".into()) },
                                Err(error) => (true, error),
                            });
                            view.refresh_theme_preview(cx);
                            cx.notify();
                        }))))
                .child(v_flex().gap_2().children(t.custom.colors.iter().map(|(role, color)| {
                    h_flex().gap_2().items_center().py_2().border_b_1().border_color(cx.theme().border).text_sm()
                        .child(div().size(px(16.)).flex_shrink_0().rounded_sm().bg(gpui_kit::component::try_parse_color(color).unwrap_or(cx.theme().muted)))
                        .child(div().flex_1().min_w_0().child(role.clone()))
                        .child(div().text_color(cx.theme().muted_foreground).child(color.clone()))
                }))))
            .child(heading("Preview"))
            .child(self.theme_preview(t))
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
        self.refresh_theme_preview(cx);
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
                                            t.appearance =
                                                Appearance::Custom(t.custom.name.clone());
                                        }
                                        view.dirty = true;
                                        view.refresh_theme_preview(cx);
                                        cx.notify();
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
                                            t.appearance =
                                                Appearance::Custom(t.custom.name.clone());
                                        }
                                        view.dirty = true;
                                        view.refresh_theme_preview(cx);
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
            .into_any_element()
    }
    fn identity_form(
        &self,
        c: &ClusterEditor,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let alias = value(&c.alias, cx);
        let name = if alias.trim().is_empty() {
            c.id.display_name().to_owned()
        } else {
            alias.trim().to_owned()
        };
        let connections = cx
            .global::<crate::connections::SharedConnections>()
            .0
            .read(cx);
        let (status, color) = if connections.sessions.contains_key(&c.id) {
            ("Connected", cx.theme().success)
        } else if connections.connecting(&c.id) {
            ("Connecting", cx.theme().warning)
        } else {
            ("Not connected", cx.theme().muted_foreground)
        };
        let columns = if window.viewport_size().width < px(800.) {
            3
        } else {
            6
        };
        let icons = [
            (ClusterIcon::Kubernetes, "Kubernetes"),
            (ClusterIcon::Server, "Server"),
            (ClusterIcon::Cloud, "Cloud"),
            (ClusterIcon::Workloads, "Workloads"),
            (ClusterIcon::Storage, "Storage"),
            (ClusterIcon::Shield, "Shield"),
        ]
        .into_iter()
        .map(|(icon, label)| {
            let selected = c.icon == icon;
            Button::new(label)
                .outline()
                .selected(selected)
                .h_auto()
                .p_3()
                .w_full()
                .bg(if selected {
                    cx.theme().link.opacity(0.09)
                } else {
                    cx.theme().background
                })
                .border_color(if selected {
                    cx.theme().link
                } else {
                    cx.theme().border
                })
                .text_color(if selected {
                    cx.theme().link
                } else {
                    cx.theme().foreground
                })
                .accessibility_label(format!("{label} cluster icon"))
                .child(
                    v_flex()
                        .items_center()
                        .gap_2()
                        .child(settings::icon(&icon).size_5())
                        .child(div().text_xs().child(label)),
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Page::Cluster(c) = &mut view.page {
                        c.icon = icon.clone();
                    }
                    view.dirty = true;
                    view.message = None;
                    cx.notify();
                }))
        });
        v_flex().gap_5()
            .child(h_flex().gap_3().p_4().rounded_lg().bg(cx.theme().muted).items_center()
                .child(div().size(px(40.)).flex_shrink_0().rounded_lg().bg(cx.theme().link.opacity(0.1)).text_color(cx.theme().link)
                    .flex().items_center().justify_center().child(settings::icon(&c.icon).size_6()))
                .child(v_flex().flex_1().min_w_0().gap_1().child(div().font_weight(FontWeight::MEDIUM)
                    .child(crate::copyable_text::copyable_text("cluster-display-name", name)))
                    .child(hint(c.id.display_name(), cx)))
                .child(h_flex().gap_1().items_center().text_xs().text_color(color)
                    .child(div().size(px(5.)).rounded_full().bg(color)).child(status)))
            .child(v_flex().gap_2().child(field("Display name", &c.alias))
                .child(hint("Used in the sidebar, tabs and status bar. Leave empty to use the kubeconfig name.", cx)))
            .child(v_flex().gap_3().child(heading("Cluster icon"))
                .child(div().grid().grid_cols(columns).gap_2().children(icons))
                .child(inline_action(Button::new("custom-cluster-icon").link().icon(IconName::Upload).label("Import SVG…")
                    .on_click(cx.listener(|view, _, window, cx| view.choose_icon(window, cx)))))
                .when(matches!(c.icon, ClusterIcon::Custom(_)), |form| form.child(h_flex().gap_2()
                    .child(settings::icon(&c.icon)).child(hint("Custom SVG icon", cx)))))
            .child(v_flex().gap_1().p_3().rounded_lg().bg(cx.theme().muted)
                .child(hint("Kubeconfig context", cx))
                .child(div().font_family(cx.theme().mono_font_family.clone()).text_sm()
                    .child(crate::copyable_text::copyable_text("cluster-context", c.id.to_string()))))
            .into_any_element()
    }

    fn metrics_form(&self, c: &ClusterEditor, cx: &mut Context<Self>) -> AnyElement {
        let choices = [
            (
                0,
                "Kubernetes Metrics API",
                "Use metrics-server from this cluster.",
            ),
            (1, "Prometheus", "Query an external Prometheus endpoint."),
            (2, "Disabled", "Do not fetch usage metrics."),
        ]
        .into_iter()
        .map(|(source, label, description)| {
            Radio::new(("metrics-source", source as usize))
                .w_full()
                .p_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .checked(c.metrics == source)
                .label(label)
                .child(hint(description, cx))
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Page::Cluster(c) = &mut view.page {
                        c.metrics = source;
                    }
                    view.dirty = true;
                    view.message = None;
                    cx.notify();
                }))
        });
        v_flex().gap_4()
            .child(v_flex().border_1().border_color(cx.theme().border).rounded_lg().overflow_hidden().children(choices))
            .when(c.metrics == 1, |form| form
                .child(field("Prometheus URL", &c.url))
                .child(hint("A URL with a base path is supported.", cx))
                .child(field("Bearer token · optional", &c.token))
                .child(v_flex().gap_4().pt_3().border_t_1().border_color(cx.theme().border)
                    .child(inline_action(Button::new("metrics-query-disclosure").ghost().icon(if self.queries_expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                        .label("Query configuration").on_click(cx.listener(|view, _, _, cx| {
                            view.queries_expanded = !view.queries_expanded;
                            cx.notify();
                        }))))
                    .when(self.queries_expanded, |queries| queries
                        .child(field("Pod CPU query", &c.pod_cpu))
                        .child(field("Pod memory query", &c.pod_memory))
                        .child(field("Node CPU query", &c.node_cpu))
                        .child(field("Node memory query", &c.node_memory))
                        .child(hint("Queries return CPU in cores and memory in bytes. Pod results require namespace/pod labels; node results require a node label. Add cluster selectors when Prometheus contains multiple clusters.", cx))))
                .child(inline_action(Button::new("test-metrics").outline().icon(IconName::Activity).disabled(self.testing)
                    .label(if self.testing { "Testing…" } else { "Test metrics source" })
                    .on_click(cx.listener(|view, _, window, cx| view.test_metrics(window, cx))))))
            .when(c.metrics == 0, |form| form.child(hint("Uses metrics.k8s.io. Clusters without metrics-server show no CPU or memory samples.", cx)))
            .into_any_element()
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let cluster = matches!(self.page, Page::Cluster(_));
        let (icon, subtitle) = match &self.page {
            Page::Application(_) => (Icon::new(IconName::RadioTower), "Preferences".to_owned()),
            Page::Cluster(c) => (settings::icon(&c.icon), c.id.display_name().to_owned()),
        };
        let sections = if cluster {
            Section::CLUSTER
        } else {
            Section::APPLICATION
        };
        v_flex()
            .w(px(180.))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .px_3()
            .py_5()
            .gap_5()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .px_1()
                    .child(
                        div()
                            .size(px(32.))
                            .flex_shrink_0()
                            .rounded_lg()
                            .bg(cx.theme().link.opacity(0.1))
                            .text_color(cx.theme().link)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon.size_5()),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().font_weight(FontWeight::MEDIUM).child(if cluster {
                                "Cluster"
                            } else {
                                "Beacon"
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(subtitle),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .px_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if cluster {
                                "THIS CLUSTER"
                            } else {
                                "APPLICATION"
                            }),
                    )
                    .children(sections.into_iter().map(|section| {
                        let selected = self.section == section;
                        Button::new(section.id())
                            .ghost()
                            .selected(selected)
                            .w_full()
                            .justify_start()
                            .h(px(38.))
                            .accessibility_label(section.label())
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .items_center()
                                    .child(Icon::new(section.icon()).size_4())
                                    .child(
                                        div().flex_1().min_w_0().truncate().child(section.label()),
                                    ),
                            )
                            .text_sm()
                            .bg(if selected {
                                cx.theme().link.opacity(0.1)
                            } else {
                                cx.theme().transparent
                            })
                            .text_color(if selected {
                                cx.theme().link
                            } else {
                                cx.theme().foreground
                            })
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.select_section(section, window, cx)
                            }))
                    })),
            )
            .child(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if cluster {
                        "Settings stored locally".to_owned()
                    } else {
                        format!("Version {}", env!("CARGO_PKG_VERSION"))
                    }),
            )
            .into_any_element()
    }
}
impl Render for PreferencesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match (&self.page, self.section) {
            (Page::Application(t), Section::Appearance) => self.appearance_form(t, cx),
            (Page::Application(t), Section::Themes) => self.theme_form(t, cx),
            (Page::Application(_), Section::Updates) => self.updates_form(cx),
            (Page::Cluster(c), Section::Identity) => self.identity_form(c, window, cx),
            (Page::Cluster(c), Section::Metrics) => self.metrics_form(c, cx),
            (Page::Cluster(c), Section::Yaml) => self.yaml_folding_form(c, cx),
            _ => self.proxy_form(cx),
        };
        let directory = settings::store(cx).read(cx).directory.clone();
        let cluster = matches!(self.page, Page::Cluster(_));
        let status = if self.dirty {
            "Unsaved changes"
        } else {
            match self.section {
                Section::Yaml => "Folding applies the next time a resource opens",
                Section::Identity => "Changes apply across cluster views after saving",
                Section::Connection | Section::Metrics => "Connection changes require a reconnect",
                Section::Updates => "Update preferences save automatically",
                _ => "Changes apply across all windows after saving",
            }
        };
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(|_: &crate::app::CloseTab, window, cx| {
                window.defer(cx, |window, _| window.remove_window())
            })
            .child(
                TitleBar::new().child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().muted_foreground)
                        .child(if cluster {
                            "Cluster settings"
                        } else {
                            "Application settings"
                        }),
                ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.sidebar(cx))
                    .child(
                        div()
                            .id(format!("preferences-scroll-{}", self.section.id()))
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(
                                v_flex()
                                    .w_full()
                                    .p_6()
                                    .gap_5()
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_size(rems(1.35))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child(self.section.title()),
                                            )
                                            .child(hint(self.section.description(), cx)),
                                    )
                                    .child(body),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .when_some(self.message.clone(), |footer, (error, message)| {
                        footer.child(
                            div()
                                .px_4()
                                .pt_3()
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
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .px_4()
                            .py_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(status),
                            )
                            .child(
                                h_flex()
                                    .flex_shrink_0()
                                    .gap_2()
                                    .when(!cluster, |bar| {
                                        bar.child(
                                            Button::new("open-settings-folder")
                                                .ghost()
                                                .icon(IconName::FolderOpen)
                                                .label("Config folder")
                                                .tooltip(directory.display().to_string())
                                                .on_click(cx.listener(move |view, _, _, cx| {
                                                    match std::fs::create_dir_all(&directory) {
                                                        Ok(()) => cx.open_with_system(&directory),
                                                        Err(error) => {
                                                            view.message =
                                                                Some((true, error.to_string()));
                                                            cx.notify();
                                                        }
                                                    }
                                                })),
                                        )
                                    })
                                    .when(cluster, |bar| {
                                        bar.child(
                                            Button::new("save-reconnect")
                                                .outline()
                                                .label("Save & reconnect")
                                                .on_click(cx.listener(|view, _, window, cx| {
                                                    view.save(true, window, cx)
                                                })),
                                        )
                                    })
                                    .child(
                                        Button::new("save-preferences")
                                            .primary()
                                            .label("Save changes")
                                            .on_click(cx.listener(|view, _, window, cx| {
                                                view.save(false, window, cx)
                                            })),
                                    ),
                            ),
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
                state.preferences.updates.auto_check = false;
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
        cx.update_window(window, |_, window, cx| {
            window.click("settings-updates", cx);
        })
        .unwrap();
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

    fn setup(cx: &mut TestAppContext, directory: &std::path::Path) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
            crate::app::init(directory.join("logs"), cx);
            settings::store(cx).update(cx, |state, _| {
                state.preferences = settings::Preferences::default();
                state.directory = directory.to_owned();
                state.load_error = None;
            });
            settings::apply(None, cx);
        });
    }

    #[::core::prelude::v1::test]
    fn cluster_settings_keep_drafts_across_sections_and_save_together() {
        let directory = tempfile::tempdir().unwrap();
        let cx = &mut TestAppContext::single();
        setup(cx, directory.path());
        let id = ClusterId::new("test-cluster");
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(880.), px(760.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| PreferencesView::new(Some(id.clone()), window, cx))
                },
            )
            .unwrap()
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                let Page::Cluster(c) = &view.page else {
                    panic!()
                };
                c.alias
                    .update(cx, |input, cx| input.set_value("Development", window, cx));
                c.alias.read(cx).focus_handle(cx).focus(window, cx);
            });
            window.click("settings-connection", cx);
            assert!(view.read(cx).focus.is_focused(window));
            window.click(("proxy-choice", 3usize), cx);
            view.update(cx, |view, cx| {
                view.proxy.url.update(cx, |input, cx| {
                    input.set_value("http://localhost:7890", window, cx)
                })
            });
            window.click("settings-metrics", cx);
            window.click(("metrics-source", 1usize), cx);
            assert!(!view.read(cx).queries_expanded);
            window.click("metrics-query-disclosure", cx);
            assert!(view.read(cx).queries_expanded);
            view.update(cx, |view, cx| {
                let Page::Cluster(c) = &view.page else {
                    panic!()
                };
                c.url.update(cx, |input, cx| {
                    input.set_value("https://prometheus.example.com", window, cx)
                });
                c.pod_cpu.update(cx, |input, cx| {
                    input.set_value("custom_cpu_query", window, cx)
                });
            });
            window.click("settings-yaml", cx);
            window.click(("yaml-fold-field", 1usize), cx);
            view.update(cx, |view, cx| {
                let Page::Cluster(c) = &view.page else {
                    panic!()
                };
                c.yaml_field.update(cx, |input, cx| {
                    input.set_value("spec.template.metadata", window, cx)
                });
            });
            window.click("add-yaml-fold-field", cx);
            window.click("settings-identity", cx);
            let Page::Cluster(c) = &view.read(cx).page else {
                panic!()
            };
            assert_eq!(value(&c.alias, cx), "Development");
            assert_eq!(value(&c.pod_cpu, cx), "custom_cpu_query");
            assert_eq!(c.yaml_folding.fields.len(), 6);
            window.click("save-preferences", cx);
        })
        .unwrap();
        cx.update(|cx| {
            let store = settings::store(cx);
            let saved = &store.read(cx).preferences.clusters[id.as_str()];
            assert_eq!(saved.alias, "Development");
            assert_eq!(
                saved.proxy,
                Some(Proxy::Custom("http://localhost:7890".into()))
            );
            let MetricsSource::Prometheus(metrics) = &saved.metrics else {
                panic!()
            };
            assert_eq!(metrics.pod_cpu, "custom_cpu_query");
            assert!(!saved.yaml_folding.fields[1].collapsed);
            assert_eq!(saved.yaml_folding.fields[5].path, "spec.template.metadata");
            assert!(!view.read(cx).dirty);
            assert!(directory.path().join("settings.json").exists());
            assert!(
                cx.global::<crate::connections::SharedConnections>()
                    .0
                    .read(cx)
                    .sessions
                    .is_empty()
            );
        });
    }

    #[::core::prelude::v1::test]
    fn custom_theme_preview_is_local_until_saved_from_another_section() {
        let directory = tempfile::tempdir().unwrap();
        let cx = &mut TestAppContext::single();
        setup(cx, directory.path());
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(880.), px(760.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| PreferencesView::new(None, window, cx))
                },
            )
            .unwrap()
        });
        cx.update_window(window, |_, window, cx| {
            let fonts = view.read(cx).fonts.clone().unwrap();
            fonts.update(cx, |fonts, _| {
                fonts._load = None;
                fonts.names = Some(vec![CustomTheme::default().mono_font_family].into());
            });
            window.render_frame(cx);
            let original = cx.theme().button_primary_foreground;
            window.click("settings-themes", cx);
            view.update(cx, |view, cx| {
                let Page::Application(t) = &view.page else {
                    panic!()
                };
                t.name
                    .update(cx, |input, cx| input.set_value("Local preview", window, cx));
                t.size
                    .update(cx, |input, cx| input.set_value("18", window, cx));
                t.color.update(cx, |input, cx| {
                    input.set_value("#11AA77", window, cx);
                    cx.emit(InputEvent::Change);
                });
            });
            assert_eq!(cx.theme().button_primary_foreground, original);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let Page::Application(t) = &view.read(cx).page else {
                panic!()
            };
            assert_eq!(t.preview.button_primary_foreground, rgb(0x11aa77).into());
            assert_eq!(t.preview.font_size, px(18.));
            assert!(settings::store(cx).read(cx).preferences.themes.is_empty());
            window.click("settings-network", cx);
            window.click(("proxy-choice", 1usize), cx);
            window.click("save-preferences", cx);
        })
        .unwrap();
        cx.update(|cx| {
            let store = settings::store(cx);
            let saved = &store.read(cx).preferences;
            assert_eq!(saved.appearance, Appearance::Custom("Local preview".into()));
            assert_eq!(saved.themes["Local preview"].font_size, 18.);
            assert_eq!(
                saved.themes["Local preview"].colors["button.primary.foreground"],
                "#11AA77"
            );
            assert_eq!(saved.proxy, Proxy::Direct);
            assert!(!view.read(cx).dirty);
        });
    }
}
