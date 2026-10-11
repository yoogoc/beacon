//! A guided theme editor. Drafts stay local until explicitly saved or applied.
use super::*;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};

type Choices = SearchableVec<String>;
type Picker = Entity<SelectState<Choices>>;
const SYSTEM_FONT: &str = "System font";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Panel {
    Colors,
    Typography,
    Advanced,
}

struct ColorRole {
    key: &'static str,
    label: &'static str,
    description: &'static str,
    group: usize,
    color: fn(&gpui_kit::component::Theme) -> Hsla,
}

fn color_hex(color: Hsla) -> String {
    let color = Rgba::from(color);
    let byte = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    let rgb = format!(
        "#{:02X}{:02X}{:02X}",
        byte(color.r),
        byte(color.g),
        byte(color.b)
    );
    if color.a < 1. {
        format!("{rgb}{:02X}", byte(color.a))
    } else {
        rgb
    }
}

fn color_control(picker: &Entity<ColorPickerState>, label: &str, cx: &App) -> AnyElement {
    ColorPicker::new(picker)
        .small()
        .w_full()
        .px_2()
        .py_1()
        .border_1()
        .border_color(cx.theme().input)
        .rounded(cx.theme().radius)
        .label(
            picker
                .read(cx)
                .value()
                .map(color_hex)
                .unwrap_or_else(|| "Use base color".into()),
        )
        .accessibility_label(label.to_string())
        .into_any_element()
}

const COLORS: &[ColorRole] = &[
    ColorRole {
        key: "background",
        label: "Page background",
        description: "Resource pages and settings",
        group: 0,
        color: |t| t.background,
    },
    ColorRole {
        key: "foreground",
        label: "Main text",
        description: "Names, headings and values",
        group: 0,
        color: |t| t.foreground,
    },
    ColorRole {
        key: "muted.foreground",
        label: "Secondary text",
        description: "Descriptions and supporting details",
        group: 0,
        color: |t| t.muted_foreground,
    },
    ColorRole {
        key: "border",
        label: "Borders",
        description: "Dividers and control outlines",
        group: 0,
        color: |t| t.border,
    },
    ColorRole {
        key: "link",
        label: "Links",
        description: "Clickable resource references",
        group: 0,
        color: |t| t.link,
    },
    ColorRole {
        key: "button.primary.background",
        label: "Primary button",
        description: "Main actions such as Save",
        group: 1,
        color: |t| t.button_primary,
    },
    ColorRole {
        key: "button.primary.foreground",
        label: "Primary button text",
        description: "Text and icons on primary buttons",
        group: 1,
        color: |t| t.button_primary_foreground,
    },
    ColorRole {
        key: "button.primary.hover.background",
        label: "Primary button hover",
        description: "Background when the pointer is over it",
        group: 1,
        color: |t| t.button_primary_hover,
    },
    ColorRole {
        key: "button.secondary.background",
        label: "Secondary button",
        description: "Supporting actions",
        group: 1,
        color: |t| t.button_secondary,
    },
    ColorRole {
        key: "button.secondary.foreground",
        label: "Secondary button text",
        description: "Text and icons on secondary buttons",
        group: 1,
        color: |t| t.button_secondary_foreground,
    },
    ColorRole {
        key: "sidebar.background",
        label: "Sidebar background",
        description: "Cluster and resource navigation",
        group: 2,
        color: |t| *t.tokens.sidebar,
    },
    ColorRole {
        key: "sidebar.foreground",
        label: "Sidebar text",
        description: "Navigation labels and icons",
        group: 2,
        color: |t| t.sidebar_foreground,
    },
    ColorRole {
        key: "sidebar.accent.background",
        label: "Selected navigation",
        description: "Background of the active item",
        group: 2,
        color: |t| *t.tokens.sidebar_accent,
    },
    ColorRole {
        key: "sidebar.accent.foreground",
        label: "Selected navigation text",
        description: "Text of the active item",
        group: 2,
        color: |t| t.sidebar_accent_foreground,
    },
    ColorRole {
        key: "success.background",
        label: "Success",
        description: "Running and healthy resources",
        group: 3,
        color: |t| t.success,
    },
    ColorRole {
        key: "warning.background",
        label: "Warning",
        description: "Pending or degraded resources",
        group: 3,
        color: |t| t.warning,
    },
    ColorRole {
        key: "danger.background",
        label: "Error",
        description: "Failed resources and error messages",
        group: 3,
        color: |t| t.danger,
    },
    ColorRole {
        key: "info.background",
        label: "Information",
        description: "Informational status and notices",
        group: 3,
        color: |t| t.info,
    },
];

fn font_label(name: &str) -> String {
    if name == ".SystemUIFont" {
        SYSTEM_FONT.into()
    } else {
        name.into()
    }
}

fn role_label(role: &str) -> String {
    role.split('.')
        .map(|part| {
            part.split('_')
                .map(|word| {
                    let mut chars = word.chars();
                    chars
                        .next()
                        .map(|first| first.to_uppercase().to_string() + chars.as_str())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join(" › ")
}

fn choices(
    items: Vec<String>,
    selected: &str,
    window: &mut Window,
    cx: &mut Context<PreferencesView>,
) -> Picker {
    cx.new(|cx| {
        let mut picker =
            SelectState::new(SearchableVec::new(items), None, window, cx).searchable(true);
        picker.set_selected_value(&selected.to_string(), window, cx);
        picker
    })
}

pub(super) struct ThemeControls {
    pub saved_name: Option<String>,
    pub dirty: bool,
    colors: BTreeMap<&'static str, Entity<ColorPickerState>>,
    advanced_color: Entity<ColorPickerState>,
    roles: Picker,
    fonts: [Picker; 2],
    panel: Panel,
    group: usize,
    width: Rc<std::cell::Cell<Pixels>>,
}

impl ThemeControls {
    pub(super) fn new(
        custom: &CustomTheme,
        preview: &gpui_kit::component::Theme,
        saved_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<PreferencesView>,
    ) -> Self {
        let colors = COLORS
            .iter()
            .map(|role| {
                (
                    role.key,
                    cx.new(|cx| {
                        ColorPickerState::new(window, cx).default_value((role.color)(preview))
                    }),
                )
            })
            .collect();
        let advanced_color = cx.new(|cx| {
            let picker = ColorPickerState::new(window, cx);
            if let Some(color) = custom
                .colors
                .get("button.primary.foreground")
                .and_then(|color| gpui_kit::component::try_parse_color(color).ok())
            {
                picker.default_value(color)
            } else {
                picker
            }
        });
        let fonts = [
            font_label(&custom.font_family),
            font_label(&custom.mono_font_family),
        ];
        let role_items = settings::color_roles()
            .iter()
            .map(|role| role_label(role))
            .collect();
        Self {
            colors,
            advanced_color,
            roles: choices(
                role_items,
                &role_label("button.primary.foreground"),
                window,
                cx,
            ),
            fonts: fonts.map(|font| {
                let mut items = vec![SYSTEM_FONT.into()];
                if font != SYSTEM_FONT {
                    items.push(font.clone());
                }
                choices(items, &font, window, cx)
            }),
            saved_name,
            dirty: false,
            panel: Panel::Colors,
            group: 0,
            width: Rc::new(std::cell::Cell::new(window.bounds().size.width - px(228.))),
        }
    }

    pub(super) fn subscriptions(
        &self,
        window: &mut Window,
        cx: &mut Context<PreferencesView>,
    ) -> Vec<Subscription> {
        let mut subscriptions = Vec::new();
        for (&role, picker) in &self.colors {
            subscriptions.push(cx.subscribe_in(
                picker,
                window,
                move |view, _, event: &ColorPickerEvent, window, cx| {
                    let ColorPickerEvent::Change(color) = event;
                    view.change_theme_color(role, *color, window, cx);
                },
            ));
        }
        subscriptions.push(cx.subscribe_in(
            &self.advanced_color,
            window,
            |view, _, event: &ColorPickerEvent, window, cx| {
                let ColorPickerEvent::Change(color) = event;
                if let Page::Application(t) = &view.page {
                    let role = t.role.clone();
                    view.change_theme_color(&role, *color, window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &self.roles,
            window,
            |view, _, event: &SelectEvent<Choices>, window, cx| {
                if let SelectEvent::Confirm(Some(label)) = event
                    && let Some(role) = settings::color_roles()
                        .iter()
                        .find(|role| role_label(role) == *label)
                {
                    view.select_color(role.clone(), window, cx);
                }
            },
        ));
        for (index, picker) in self.fonts.iter().enumerate() {
            subscriptions.push(cx.subscribe_in(
                picker,
                window,
                move |view, _, event: &SelectEvent<Choices>, window, cx| {
                    if let SelectEvent::Confirm(Some(font)) = event {
                        if let Page::Application(t) = &mut view.page {
                            let input = if index == 0 { &t.font } else { &t.mono };
                            let font = if font == SYSTEM_FONT {
                                ".SystemUIFont"
                            } else {
                                font
                            };
                            input.update(cx, |input, cx| {
                                input.set_value(font.to_string(), window, cx)
                            });
                        }
                        view.theme_changed(window, cx);
                    }
                },
            ));
        }
        subscriptions
    }
}

impl PreferencesView {
    fn theme_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Page::Application(t) = &mut self.page {
            t.controls.dirty = true;
            t.appearance = Appearance::Custom(t.custom.name.clone());
        }
        self.dirty = true;
        self.message = None;
        self.refresh_theme_preview(cx);
        self.sync_theme_pickers(window, cx);
        cx.notify();
    }

    fn change_theme_color(
        &mut self,
        role: &str,
        color: Option<Hsla>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Page::Application(t) = &mut self.page {
            let value = color.map(color_hex);
            if let Some(value) = &value {
                t.custom.colors.insert(role.into(), value.clone());
            } else {
                t.custom.colors.remove(role);
            }
            if t.role == role {
                t.color.update(cx, |input, cx| {
                    input.set_value(value.unwrap_or_default(), window, cx)
                });
            }
        }
        self.theme_changed(window, cx);
    }

    pub(super) fn sync_theme_pickers(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Application(t) = &self.page else {
            return;
        };
        for role in COLORS {
            let color = (role.color)(&t.preview);
            let picker = &t.controls.colors[role.key];
            if picker.read(cx).value() != Some(color) {
                picker.update(cx, |picker, cx| picker.set_value(color, window, cx));
            }
        }
        let color = gpui_kit::component::try_parse_color(&value(&t.color, cx)).ok();
        let picker = &t.controls.advanced_color;
        if picker.read(cx).value() != color {
            picker.update(cx, |picker, cx| {
                if let Some(color) = color {
                    picker.set_value(color, window, cx);
                } else {
                    picker.clear_value(window, cx);
                }
            });
        }
    }

    pub(super) fn sync_font_choices(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Application(t) = &self.page else {
            return;
        };
        let mut names = vec![SYSTEM_FONT.to_string()];
        if let Some(fonts) = &self.fonts
            && let Some(loaded) = &fonts.read(cx).names
        {
            names.extend(loaded.iter().filter(|name| !name.starts_with('.')).cloned());
        }
        for (picker, input) in t.controls.fonts.iter().zip([&t.font, &t.mono]) {
            let selected = font_label(&value(input, cx));
            let mut items = names.clone();
            if !items.contains(&selected) {
                items.push(selected.clone());
            }
            picker.update(cx, |picker, cx| {
                picker.set_items(SearchableVec::new(items), window, cx);
                picker.set_selected_value(&selected, window, cx);
            });
        }
    }

    fn select_color(&mut self, role: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.sync_color(cx) {
            self.message = Some((true, error));
            if let Page::Application(t) = &self.page {
                t.controls.roles.update(cx, |picker, cx| {
                    picker.set_selected_value(&role_label(&t.role), window, cx);
                });
            }
            cx.notify();
            return;
        }
        if let Page::Application(t) = &mut self.page {
            let color = t.custom.colors.get(&role).cloned().unwrap_or_default();
            t.role = role;
            t.color
                .update(cx, |input, cx| input.set_value(color, window, cx));
            t.controls.roles.update(cx, |picker, cx| {
                picker.set_selected_value(&role_label(&t.role), window, cx)
            });
        }
        self.message = None;
        self.refresh_theme_preview(cx);
        self.sync_theme_pickers(window, cx);
        cx.notify();
    }

    pub(super) fn request_theme(
        &mut self,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Page::Application(t) = &self.page else {
            return;
        };
        if t.controls.dirty {
            let answer = window.prompt(
                PromptLevel::Warning,
                "Discard unsaved theme changes?",
                Some("Save the current theme first if you want to keep your edits."),
                &["Cancel", "Discard changes"],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                if answer.await.ok() == Some(1) {
                    let _ =
                        this.update_in(cx, |view, window, cx| view.load_theme(name, window, cx));
                }
            })
            .detach();
        } else {
            self.load_theme(name, window, cx);
        }
    }

    fn load_theme(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let preferences = settings::store(cx).read(cx).preferences.clone();
        let custom = if let Some(name) = &name {
            let Some(custom) = preferences.themes.get(name) else {
                return;
            };
            custom.clone()
        } else {
            let mut name = "My theme".to_string();
            let mut number = 2;
            while preferences.themes.contains_key(&name) {
                name = format!("My theme {number}");
                number += 1;
            }
            CustomTheme {
                name,
                dark: matches!(preferences.appearance, Appearance::Dark)
                    || matches!(&self.page, Page::Application(t) if t.custom.dark),
                ..Default::default()
            }
        };
        if let Page::Application(t) = &mut self.page {
            for (input, value) in [
                (&t.name, custom.name.clone()),
                (&t.font, custom.font_family.clone()),
                (&t.size, custom.font_size.to_string()),
                (&t.mono, custom.mono_font_family.clone()),
                (&t.mono_size, custom.mono_font_size.to_string()),
                (
                    &t.color,
                    custom.colors.get(&t.role).cloned().unwrap_or_default(),
                ),
            ] {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
            t.appearance = Appearance::Custom(custom.name.clone());
            t.custom = custom;
            t.controls.dirty = name.is_none();
            t.controls.saved_name = name;
        }
        self.message = None;
        self.dirty = true;
        self.refresh_theme_preview(cx);
        self.sync_theme_pickers(window, cx);
        self.sync_font_choices(window, cx);
        if let Page::Application(t) = &self.page {
            t.name.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    pub(super) fn theme_apply_label(&self) -> &'static str {
        match &self.page {
            Page::Application(t) if t.controls.saved_name.is_none() => "Create & apply",
            Page::Application(t) if !t.controls.dirty => "Apply theme",
            _ => "Save & apply",
        }
    }

    pub(super) fn theme_save_label(&self, cx: &App) -> &'static str {
        if let Page::Application(t) = &self.page
            && settings::store(cx).read(cx).preferences.appearance
                == Appearance::Custom(value(&t.name, cx).trim().into())
        {
            "Save a copy"
        } else {
            "Save theme"
        }
    }

    pub(super) fn save_custom_theme(
        &mut self,
        apply: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = (|| {
            let mut custom = self.read_custom_theme(cx)?;
            let Page::Application(t) = &self.page else {
                return Ok(());
            };
            let mut preferences = settings::store(cx).read(cx).preferences.clone();
            // Updating a theme currently in use would also apply it. The
            // secondary action explicitly saves a copy in that case.
            if !apply && preferences.appearance == Appearance::Custom(custom.name.clone()) {
                let stem = format!("{} copy", custom.name);
                custom.name = stem.clone();
                let mut number = 2;
                while preferences.themes.contains_key(&custom.name) {
                    custom.name = format!("{stem} {number}");
                    number += 1;
                }
            }
            if preferences.themes.contains_key(&custom.name)
                && t.controls.saved_name.as_ref() != Some(&custom.name)
            {
                return Err("A theme with this name already exists. Choose another name, or select the saved theme to edit it.".into());
            }
            let name = custom.name.clone();
            preferences.themes.insert(name.clone(), custom.clone());
            if apply {
                preferences.appearance = Appearance::Custom(name.clone());
            }
            settings::save(preferences.clone(), window, cx)?;
            if let Page::Application(t) = &mut self.page {
                t.name
                    .update(cx, |input, cx| input.set_value(name.clone(), window, cx));
                t.custom = custom;
                t.controls.saved_name = Some(name);
                t.controls.dirty = false;
                t.appearance = preferences.appearance;
            }
            self.dirty = self.proxy.read(cx).ok().flatten() != Some(preferences.proxy);
            self.refresh_theme_preview(cx);
            self.sync_theme_pickers(window, cx);
            Ok::<_, String>(())
        })();
        self.message = Some(match result {
            Ok(()) => (
                false,
                if apply {
                    "Theme saved and applied across all windows."
                } else {
                    "Theme saved locally. Click Apply theme to use it."
                }
                .into(),
            ),
            Err(error) => (true, error),
        });
        cx.notify();
    }

    fn draft_preview(&self, t: &ThemeEditor) -> AnyElement {
        let theme = &t.preview;
        v_flex()
            .id("custom-theme-preview")
            .test_support()
            .w_full()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .text_size(theme.font_size * 0.75)
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().size(px(5.)).rounded_full().bg(theme.success))
                    .child("Beacon")
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Live preview"),
                    ),
            )
            .child(
                h_flex()
                    .h(px(100.))
                    .items_stretch()
                    .child(
                        v_flex()
                            .w(px(72.))
                            .p_2()
                            .gap_2()
                            .bg(theme.tokens.sidebar)
                            .text_color(theme.sidebar_foreground)
                            .child(
                                div()
                                    .rounded_sm()
                                    .px_1()
                                    .bg(theme.tokens.sidebar_accent)
                                    .text_color(theme.sidebar_accent_foreground)
                                    .child("Pods"),
                            )
                            .child("Nodes")
                            .child("Events"),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .p_2()
                            .gap_2()
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(div().font_weight(FontWeight::MEDIUM).child("Pods"))
                                    .child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .bg(theme.button_primary)
                                            .text_color(theme.button_primary_foreground)
                                            .child("Create"),
                                    ),
                            )
                            .child(div().text_color(theme.muted_foreground).child("Name"))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .justify_between()
                                    .child(
                                        div()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_size(theme.mono_font_size * 0.75)
                                            .child("api-server"),
                                    )
                                    .child(div().text_color(theme.success).child("Running")),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn color_cards(&self, t: &ThemeEditor, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .items_stretch()
            .flex_wrap()
            .gap_2()
            .children(
                COLORS
                    .iter()
                    .filter(|role| role.group == t.controls.group)
                    .map(|role| {
                        let overridden = t.custom.colors.contains_key(role.key);
                        let key = role.key;
                        v_flex()
                            .id(format!("theme-color-{key}"))
                            .test_support()
                            .flex_1()
                            .min_w(px(210.))
                            .flex_basis(relative(if t.controls.width.get() < px(460.) {
                                1.
                            } else {
                                0.46
                            }))
                            .gap_2()
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(
                                v_flex().gap_1().child(heading(role.label)).child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(role.description),
                                ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(color_control(&t.controls.colors[key], role.label, cx))
                                    .child(
                                        Button::new(format!("reset-theme-color-{key}"))
                                            .small()
                                            .ghost()
                                            .icon(IconName::RotateCcw)
                                            .accessibility_label(format!(
                                                "Reset {} to base color",
                                                role.label
                                            ))
                                            .tooltip("Use base color")
                                            .disabled(!overridden)
                                            .on_click(cx.listener(move |view, _, window, cx| {
                                                view.change_theme_color(key, None, window, cx)
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if overridden {
                                        "Custom color"
                                    } else {
                                        "Inherited from base"
                                    }),
                            )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn theme_form(&self, t: &ThemeEditor, cx: &Context<Self>) -> AnyElement {
        let bases = [(false, "Light"), (true, "Dark")]
            .into_iter()
            .map(|(dark, label)| {
                Button::new(format!("theme-base-{label}"))
                    .small()
                    .outline()
                    .selected(t.custom.dark == dark)
                    .label(label)
                    .on_click(cx.listener(move |view, _, window, cx| {
                        if let Page::Application(t) = &mut view.page {
                            t.custom.dark = dark;
                        }
                        view.theme_changed(window, cx);
                    }))
            });
        let tabs = [
            (Panel::Colors, "Colors"),
            (Panel::Typography, "Typography"),
            (Panel::Advanced, "Advanced"),
        ]
        .into_iter()
        .map(|(panel, label)| {
            Button::new(format!("theme-panel-{label}"))
                .small()
                .ghost()
                .selected(t.controls.panel == panel)
                .label(label)
                .on_click(cx.listener(move |view, _, window, cx| {
                    if let Page::Application(t) = &mut view.page {
                        t.controls.panel = panel;
                    }
                    view.sync_font_choices(window, cx);
                    cx.notify();
                }))
        });
        let groups = ["Interface", "Buttons", "Sidebar", "Status"]
            .into_iter()
            .enumerate()
            .map(|(group, label)| {
                Button::new(("theme-color-group", group))
                    .small()
                    .outline()
                    .selected(t.controls.group == group)
                    .label(label)
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Page::Application(t) = &mut view.page {
                            t.controls.group = group;
                        }
                        cx.notify();
                    }))
            });
        let panel = match t.controls.panel {
            Panel::Colors => v_flex().gap_3().child(h_flex().gap_2().flex_wrap().children(groups))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Click a color to choose from the palette or enter a hex value. The preview updates immediately."))
                .child(self.color_cards(t, cx)).into_any_element(),
            Panel::Typography => v_flex().gap_4()
                .child(hint("Choose an installed font. Sizes are in pixels (10–30).", cx))
                .children([("Interface font", &t.size, &t.controls.fonts[0]), ("Monospace font", &t.mono_size, &t.controls.fonts[1])].into_iter().map(|(label, size, picker)| {
                    h_flex().items_end().gap_3()
                        .child(v_flex().flex_1().min_w_0().gap_2().child(heading(label)).child(Select::new(picker).w_full().search_placeholder("Search installed fonts").accessibility_label(label)))
                        .child(div().w(px(85.)).child(field("Size · px", size)))
                })).into_any_element(),
            Panel::Advanced => v_flex().gap_3()
                .child(hint("All color options. Search by component, such as button or sidebar.", cx))
                .child(Select::new(&t.controls.roles).w_full().search_placeholder("Search color options").accessibility_label("Advanced color option"))
                .child(h_flex().gap_2().items_center()
                    .child(color_control(&t.controls.advanced_color, "Advanced color", cx))
                    .child(Button::new("reset-advanced-color").outline().label("Use base color").on_click(cx.listener(|view, _, window, cx| {
                        if let Page::Application(t) = &view.page { let role = t.role.clone(); view.change_theme_color(&role, None, window, cx); }
                    }))))
                .child(field("Hex value · Leave empty to inherit", &t.color))
                .child(hint(&t.role, cx))
                .child(heading("Your custom colors"))
                .when(t.custom.colors.is_empty(), |panel| panel.child(hint("No overrides yet. All colors inherit from the base theme.", cx)))
                .children(t.custom.colors.iter().map(|(role, color)| {
                    let key = role.clone();
                    Button::new(format!("theme-override-{role}")).ghost().w_full().justify_start().label(format!("{} · {}", role_label(role), color))
                        .on_click(cx.listener(move |view, _, window, cx| view.select_color(key.clone(), window, cx)))
                })).into_any_element(),
        };
        let width = t.controls.width.clone();
        let entity = cx.entity().entity_id();
        let basis = relative(if width.get() < px(520.) { 1. } else { 0.46 });
        v_flex()
            .id("custom-theme-editor")
            .test_support()
            .relative()
            .w_full()
            .min_w_0()
            .on_prepaint(move |bounds, _, cx| {
                if width.replace(bounds.size.width) != bounds.size.width {
                    cx.notify(entity);
                }
            })
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.saved_theme_picker(t, cx)),
                    )
                    .child(
                        Button::new("new-custom-theme")
                            .outline()
                            .icon(IconName::Plus)
                            .label("New theme")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.request_theme(None, window, cx)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .flex_wrap()
                    .items_center()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .flex_basis(basis)
                            .gap_3()
                            .child(field("Theme name", &t.name))
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(heading("Base"))
                                    .child(h_flex().gap_1().children(bases)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if t.controls.saved_name.is_none() {
                                        "New theme · Not saved yet"
                                    } else if t.controls.dirty {
                                        "Saved theme · Unsaved edits"
                                    } else {
                                        "Saved locally on this device"
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex_basis(basis)
                            .child(self.draft_preview(t)),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .pb_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(tabs),
            )
            .child(panel)
            .into_any_element()
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;

    fn fixture(
        directory: &std::path::Path,
    ) -> (TestAppContext, AnyWindowHandle, Entity<PreferencesView>) {
        let mut cx = TestAppContext::single();
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
            window.click("settings-themes", cx);
        })
        .unwrap();
        (cx, window, view)
    }

    #[::core::prelude::v1::test]
    fn common_colors_save_without_applying_or_saving_network_drafts() {
        let directory = tempfile::tempdir().unwrap();
        let (mut cx, window, view) = fixture(directory.path());
        let original = cx.update(|cx| cx.theme().background);
        cx.update_window(window, |_, window, cx| {
            let Page::Application(t) = &view.read(cx).page else {
                panic!()
            };
            let picker = t.controls.colors["background"].clone();
            picker.update(cx, |picker, cx| {
                picker.select_color(rgb(0xeef2f6).into(), window, cx)
            });
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            let Page::Application(t) = &view.read(cx).page else {
                panic!()
            };
            assert_eq!(t.custom.colors["background"], "#EEF2F6");
            assert_eq!(t.preview.background, rgb(0xeef2f6).into());
            assert_eq!(cx.theme().background, original);
            view.update(cx, |view, _| {
                view.proxy.choice = ProxyChoice::Direct;
                view.dirty = true;
            });
            window.render_frame(cx);
            assert!(window.try_find("save-preferences").is_none());
            window.click("save-theme", cx);
            let store = settings::store(cx);
            let saved = &store.read(cx).preferences;
            assert_eq!(saved.appearance, Appearance::Light);
            assert_eq!(saved.proxy, Proxy::System);
            assert_eq!(saved.themes["My theme"].colors["background"], "#EEF2F6");
            assert_eq!(cx.theme().background, original);
            assert!(view.read(cx).dirty);
            assert_eq!(view.read(cx).theme_apply_label(), "Apply theme");
            window.click("apply-theme", cx);
            assert_eq!(cx.theme().background, rgb(0xeef2f6).into());
            assert_eq!(
                store.read(cx).preferences.appearance,
                Appearance::Custom("My theme".into())
            );
            assert_eq!(store.read(cx).preferences.proxy, Proxy::System);
            let disk: settings::Preferences = serde_json::from_slice(
                &std::fs::read(directory.path().join("settings.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(disk.appearance, Appearance::Custom("My theme".into()));
            assert_eq!(disk.themes["My theme"].colors["background"], "#EEF2F6");
            view.update(cx, |view, cx| {
                view.change_theme_color("background", Some(rgb(0xd0deff).into()), window, cx);
            });
            assert_eq!(view.read(cx).theme_save_label(cx), "Save a copy");
            window.click("save-theme", cx);
            assert_eq!(cx.theme().background, rgb(0xeef2f6).into());
            assert_eq!(
                store.read(cx).preferences.themes["My theme"].colors["background"],
                "#EEF2F6"
            );
            assert_eq!(
                store.read(cx).preferences.themes["My theme copy"].colors["background"],
                "#D0DEFF"
            );
            window.click("settings-network", cx);
            window.click("save-preferences", cx);
            assert_eq!(store.read(cx).preferences.proxy, Proxy::Direct);
            assert_eq!(
                store.read(cx).preferences.appearance,
                Appearance::Custom("My theme".into())
            );
            assert_eq!(cx.theme().background, rgb(0xeef2f6).into());
        })
        .unwrap();
    }

    #[::core::prelude::v1::test]
    fn reset_and_base_changes_preserve_other_color_overrides() {
        let directory = tempfile::tempdir().unwrap();
        let (mut cx, window, view) = fixture(directory.path());
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.change_theme_color(
                    "button.primary.foreground",
                    Some(rgb(0x11aa77).into()),
                    window,
                    cx,
                );
                view.change_theme_color("link", Some(rgb(0x3388ff).into()), window, cx);
            });
            window.render_frame(cx);
            window.click("theme-base-Dark", cx);
            let Page::Application(t) = &view.read(cx).page else {
                panic!()
            };
            assert!(t.custom.dark);
            assert_eq!(t.preview.button_primary_foreground, rgb(0x11aa77).into());
            assert_eq!(t.preview.link, rgb(0x3388ff).into());
            window.click(("theme-color-group", 1usize), cx);
            window.click("reset-theme-color-button.primary.foreground", cx);
            let Page::Application(t) = &view.read(cx).page else {
                panic!()
            };
            assert!(!t.custom.colors.contains_key("button.primary.foreground"));
            assert!(value(&t.color, cx).is_empty());
            assert_eq!(t.preview.link, rgb(0x3388ff).into());
            assert_eq!(
                t.controls.colors["button.primary.foreground"]
                    .read(cx)
                    .value(),
                Some(t.preview.button_primary_foreground)
            );
            assert_eq!(
                t.preview.button_primary_foreground,
                t.dark_preview.button_primary_foreground
            );
            for role in COLORS {
                assert!(
                    settings::color_roles()
                        .binary_search(&role.key.to_string())
                        .is_ok(),
                    "{}",
                    role.key
                );
            }
        })
        .unwrap();
    }

    #[::core::prelude::v1::test]
    fn new_themes_cannot_overwrite_saved_names_and_switching_preserves_drafts_on_cancel() {
        let directory = tempfile::tempdir().unwrap();
        let (mut cx, window, view) = fixture(directory.path());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("save-theme", cx);
            window.click("new-custom-theme", cx);
            let Page::Application(t) = &view.read(cx).page else {
                panic!()
            };
            assert_eq!(value(&t.name, cx), "My theme 2");
            let name = t.name.clone();
            name.update(cx, |input, cx| input.set_value("My theme", window, cx));
            view.update(cx, |view, cx| {
                view.change_theme_color("background", Some(rgb(0x223344).into()), window, cx)
            });
            window.click("save-theme", cx);
            assert!(view.read(cx).message.as_ref().unwrap().0);
            assert!(
                settings::store(cx).read(cx).preferences.themes["My theme"]
                    .colors
                    .is_empty()
            );
            view.update(cx, |view, cx| {
                view.select_theme("My theme".into(), window, cx)
            });
        })
        .unwrap();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            let Page::Application(t) = &view.read(cx).page else {
                panic!()
            };
            assert_eq!(t.custom.colors["background"], "#223344");
            assert!(t.controls.dirty);
            view.update(cx, |view, cx| {
                view.select_theme("My theme".into(), window, cx)
            });
        })
        .unwrap();
        cx.simulate_prompt_answer("Discard changes");
        cx.run_until_parked();
        cx.update(|cx| {
            let Page::Application(t) = &view.read(cx).page else {
                panic!()
            };
            assert!(t.custom.colors.is_empty());
            assert!(!t.controls.dirty);
            assert_eq!(t.controls.saved_name.as_deref(), Some("My theme"));
        });
    }

    #[::core::prelude::v1::test]
    fn grouped_colors_and_controls_fit_light_dark_and_narrow_windows() {
        let directory = tempfile::tempdir().unwrap();
        let (mut cx, window, view) = fixture(directory.path());
        for appearance in [Appearance::Light, Appearance::Dark] {
            cx.update(|cx| {
                settings::store(cx)
                    .update(cx, |store, _| store.preferences.appearance = appearance);
                settings::apply(None, cx);
            });
            for width in [650., 880., 1100.] {
                for group in 0..4 {
                    cx.update_window(window, |_, window, cx| {
                        window.resize(size(px(width), px(1500.)));
                        window.bounds_changed(cx);
                        view.update(cx, |view, _| {
                            let Page::Application(t) = &mut view.page else {
                                panic!()
                            };
                            t.controls.group = group;
                        });
                        window.render_frame(cx);
                    })
                    .unwrap();
                    cx.run_until_parked();
                    cx.update_window(window, |_, window, cx| {
                        window.render_frame(cx);
                        let editor = window.find("custom-theme-editor").bounds();
                        for role in COLORS.iter().filter(|role| role.group == group) {
                            let card = window.find(format!("theme-color-{}", role.key));
                            assert!(card.bounds().size.width > px(0.));
                            assert!(
                                card.bounds().right() <= editor.right(),
                                "{} at {width}: {:?} outside {:?}",
                                role.key,
                                card.bounds(),
                                editor
                            );
                            let reset = window.find(format!("reset-theme-color-{}", role.key));
                            assert!(
                                reset.bounds().right() <= card.bounds().right(),
                                "{} reset overflows at {width}",
                                role.key
                            );
                        }
                        assert!(
                            editor.right() <= px(width),
                            "editor at {width}, group {group}: {editor:?}, viewport {:?}",
                            window.viewport_size()
                        );
                    })
                    .unwrap();
                }
                cx.update_window(window, |_, window, cx| {
                    window.click("theme-panel-Typography", cx);
                    window.render_frame(cx);
                    window.click("theme-panel-Advanced", cx);
                    window.render_frame(cx);
                    window.click("theme-panel-Colors", cx);
                })
                .unwrap();
            }
        }
    }
}
