//! Application language and display-only translation. API values and IDs never
//! pass through this module. Catalogs are embedded at compile time.
use gpui_kit::{component::input::InputState, *};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    sync::LazyLock,
};

// UI translation belongs to GPUI's foreground thread. Explicit locale lookups
// also isolate separate TestAppContexts running on different test threads.
thread_local! { static LOCALE: Cell<&'static str> = const { Cell::new("en") }; }

include!(concat!(env!("OUT_DIR"), "/messages.rs"));

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Language {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "en", other)]
    English,
}

impl Language {
    pub const ALL: [Self; 3] = [Self::System, Self::English, Self::SimplifiedChinese];

    pub fn label(self) -> SharedString {
        match self {
            Self::System => text("Follow system"),
            Self::English => "English".into(),
            Self::SimplifiedChinese => "简体中文".into(),
        }
    }

    pub fn resolve(self, system: Option<&str>) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
            Self::System => {
                let locale = system
                    .unwrap_or("en")
                    .replace('_', "-")
                    .to_ascii_lowercase();
                let parts: Vec<_> = locale.split(['-', '.', '@']).collect();
                if parts.first() == Some(&"zh")
                    && (parts.contains(&"hans")
                        || !parts
                            .iter()
                            .any(|part| matches!(*part, "hant" | "tw" | "hk" | "mo")))
                {
                    "zh-CN"
                } else {
                    "en"
                }
            }
        }
    }
}

pub(crate) struct LocaleState {
    pub locale: &'static str,
    titles: RefCell<HashMap<AnyWindowHandle, String>>,
}
impl Global for LocaleState {}

pub(crate) fn init(language: Language, cx: &mut App) {
    let system = sys_locale::get_locale();
    let locale = language.resolve(system.as_deref());
    gpui_kit::component::set_locale(locale);
    LOCALE.set(locale);
    cx.set_global(LocaleState {
        locale,
        titles: RefCell::new(HashMap::new()),
    });
}

pub(crate) fn apply(language: Language, cx: &mut App) {
    let system = sys_locale::get_locale();
    let locale = language.resolve(system.as_deref());
    if cx.try_global::<LocaleState>().is_none() {
        init(language, cx);
    }
    if cx.global::<LocaleState>().locale == locale {
        return;
    }
    gpui_kit::component::set_locale(locale);
    LOCALE.set(locale);
    cx.global_mut::<LocaleState>().locale = locale;
    crate::app::rebuild_menus(cx);
    // The window initiating the change is temporarily borrowed by GPUI.
    // Update native titles after that callback returns, including that window.
    cx.defer(|cx| {
        let titles = cx.global::<LocaleState>().titles.borrow().clone();
        let windows = cx.windows();
        for (handle, title) in titles {
            if windows.contains(&handle) {
                let _ = handle.update(cx, |_, window, _| {
                    window.set_window_title(&title_text(&title))
                });
            } else {
                cx.global::<LocaleState>()
                    .titles
                    .borrow_mut()
                    .remove(&handle);
            }
        }
    });
    cx.refresh_windows();
}

/// Catalog lookup for existing canonical labels. Both maps are initialized once;
/// rendering clones SharedStrings instead of parsing or allocating translations.
pub(crate) fn text(source: impl AsRef<str>) -> SharedString {
    static EN: LazyLock<HashMap<&'static str, SharedString>> = LazyLock::new(|| {
        MESSAGES
            .iter()
            .map(|(source, _)| (*source, SharedString::from(*source)))
            .collect()
    });
    static ZH: LazyLock<HashMap<&'static str, SharedString>> = LazyLock::new(|| {
        MESSAGES
            .iter()
            .map(|(source, key)| {
                (
                    *source,
                    SharedString::from(rust_i18n::t!(*key, locale = "zh-CN").into_owned()),
                )
            })
            .collect()
    });
    let source = source.as_ref();
    let messages = if LOCALE.get() == "zh-CN" { &*ZH } else { &*EN };
    messages
        .get(source)
        .cloned()
        .unwrap_or_else(|| source.to_owned().into())
}

pub(crate) fn message(key: &str, args: &[(&str, String)]) -> SharedString {
    let template = rust_i18n::t!(key, locale = LOCALE.get());
    let patterns: Vec<_> = args.iter().map(|(key, _)| *key).collect();
    let values: Vec<_> = args.iter().map(|(_, value)| value.clone()).collect();
    rust_i18n::replace_patterns(&template, &patterns, &values).into()
}

pub(crate) fn filter_text(source: &str) -> SharedString {
    if let Some((label, value)) = source.split_once(": ") {
        format!("{}: {}", text(label), text(value)).into()
    } else {
        text(source)
    }
}

pub(crate) fn follow_system<T: 'static>(window: &mut Window, cx: &mut Context<T>) {
    cx.observe_window_activation(window, |_, window, cx| {
        if window.is_window_active()
            && crate::settings::store(cx).read(cx).preferences.language == Language::System
        {
            apply(Language::System, cx);
        }
    })
    .detach();
}

pub(crate) fn window_title(source: impl AsRef<str>, window: &mut Window, cx: &mut App) {
    let source = source.as_ref();
    window.set_window_title(&title_text(source));
    if cx.try_global::<LocaleState>().is_some() {
        cx.global::<LocaleState>()
            .titles
            .borrow_mut()
            .insert(window.window_handle(), source.to_owned());
    }
}

pub(crate) fn title_text(source: &str) -> SharedString {
    if let Some(operation) = source.strip_prefix("Beacon — Helm ") {
        return format!("Beacon — Helm {}", text(operation)).into();
    }
    for (prefix, label) in [
        ("Beacon — Cluster settings · ", "Cluster settings"),
        ("Beacon — Compare ", "Compare"),
        ("Used by · ", "Used by"),
    ] {
        if let Some(suffix) = source.strip_prefix(prefix) {
            let translated = text(label);
            return if prefix.starts_with("Beacon — Cluster") {
                format!("Beacon — {translated} · {suffix}").into()
            } else if prefix.starts_with("Beacon —") {
                format!("Beacon — {translated} {suffix}").into()
            } else {
                format!("{translated} · {suffix}").into()
            };
        }
    }
    text(source)
}

#[cfg(test)]
mod tests;

/// Keep placeholders reactive without replacing the input, its value, selection,
/// completion state, drafts or undo history.
pub(crate) fn input(
    source: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<InputState>,
) -> InputState {
    let source = source.into();
    let placeholder = text(&source);
    cx.observe_global_in::<LocaleState>(window, move |input, window, cx| {
        input.set_placeholder(text(&source), window, cx);
    })
    .detach();
    InputState::new(window, cx).placeholder(placeholder)
}

pub(crate) fn table<D: gpui_kit::component::table::TableDelegate + 'static>(
    delegate: D,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::table::TableState<D>>,
) -> gpui_kit::component::table::TableState<D> {
    cx.observe_global_in::<LocaleState>(window, |table, _, cx| {
        table.refresh(cx);
        cx.notify();
    })
    .detach();
    gpui_kit::component::table::TableState::new(delegate, window, cx)
}
