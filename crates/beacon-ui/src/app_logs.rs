//! The app's own logs, independent of Kubernetes connections.

mod file;

use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Root, Sizable as _, TitleBar, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme::{BeaconTheme as _, Tone};
use file::{Filter, Level, Line, Snapshot};

/// The handle does not keep the closed window's view or polling task alive.
struct LogWindow {
    directory: PathBuf,
    window: Option<WindowHandle<Root>>,
}

impl Global for LogWindow {}

pub(crate) fn init(directory: PathBuf, cx: &mut App) {
    cx.set_global(LogWindow {
        directory,
        window: None,
    });
}

pub(crate) fn open(cx: &mut App) {
    // Run after action dispatch, when even the currently focused log window can
    // safely be updated. Multiple requests share the same window handle.
    cx.defer(|cx| {
        if let Some(handle) = cx.global::<LogWindow>().window {
            if handle.read(cx).is_ok()
                && handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
            {
                return;
            }
            cx.global_mut::<LogWindow>().window = None;
        }

        let directory = cx.global::<LogWindow>().directory.clone();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1000.), px(600.)),
                cx,
            ))),
            window_min_size: Some(size(px(640.), px(360.))),
            ..TitleBar::window_options()
        };
        match cx.open_window(options, |window, cx| {
            crate::i18n::window_title("Beacon — App logs", window, cx);
            let view = cx.new(|cx| AppLogs::new(directory, window, cx));
            window.activate_window();
            cx.new(|cx| Root::new(view, window, cx))
        }) {
            Ok(handle) => cx.global_mut::<LogWindow>().window = Some(handle),
            Err(error) => tracing::error!(%error, "could not open app logs window"),
        }
    });
}

pub(crate) struct AppLogs {
    focus: FocusHandle,
    directory: PathBuf,
    snapshot: Option<Snapshot>,
    error: Option<String>,
    visible: Arc<Vec<Line>>,
    search: Entity<InputState>,
    query: String,
    filter: Filter,
    paused: bool,
    scroll: UniformListScrollHandle,
    measurement: Option<LineMeasurement>,
    desktop_scrollbars: bool,
    _refresh: Task<()>,
    _search: Subscription,
}

struct LineMeasurement {
    lines: Arc<Vec<Line>>,
    font: Font,
    size: Pixels,
    widest: Option<usize>,
}

impl Focusable for AppLogs {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AppLogs {
    pub fn new(directory: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let search = cx.new(|cx| crate::i18n::input("Search app logs", window, cx));
        let search_events =
            cx.subscribe_in(&search, window, |view, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    view.query = input.read(cx).value().to_lowercase();
                    view.rebuild();
                    cx.notify();
                }
            });
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                let Ok(request) = this.update(cx, |view: &mut Self, _| {
                    (!view.paused).then(|| {
                        (
                            view.directory.clone(),
                            view.snapshot
                                .as_ref()
                                .and_then(|snapshot| snapshot.stamp.clone()),
                        )
                    })
                }) else {
                    return;
                };
                if let Some((directory, stamp)) = request {
                    let reading = cx
                        .background_executor()
                        .spawn(async move { file::read_latest(&directory, stamp.as_ref()) });
                    let result = reading.await;
                    if this
                        .update(cx, |view, cx| {
                            if view.paused {
                                return;
                            }
                            match result {
                                Ok(Some(snapshot)) => {
                                    let changed = view.error.is_some()
                                        || view.snapshot.as_ref().is_none_or(|previous| {
                                            previous.stamp != snapshot.stamp
                                        });
                                    view.error = None;
                                    view.snapshot = Some(snapshot);
                                    if changed {
                                        view.rebuild();
                                        cx.notify();
                                    }
                                }
                                Ok(None) => {
                                    if view.error.take().is_some() {
                                        cx.notify();
                                    }
                                }
                                Err(error) => {
                                    if view.error.as_ref() != Some(&error) {
                                        view.error = Some(error);
                                        cx.notify();
                                    }
                                }
                            }
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        });
        Self {
            focus,
            directory,
            snapshot: None,
            error: None,
            visible: Arc::new(Vec::new()),
            search,
            query: String::new(),
            filter: Filter::All,
            paused: false,
            scroll: UniformListScrollHandle::new(),
            measurement: None,
            desktop_scrollbars: crate::scroll::DESKTOP_SCROLLBARS,
            _refresh: refresh,
            _search: search_events,
        }
    }

    fn rebuild(&mut self) {
        self.visible = Arc::new(
            self.snapshot
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .lines
                        .iter()
                        .filter(|line| self.filter.accepts(line, &self.query))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        );
        if !self.paused && !self.visible.is_empty() {
            self.scroll
                .scroll_to_item(self.visible.len() - 1, ScrollStrategy::Bottom);
        }
    }
}

impl Render for AppLogs {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font = Font {
            family: "monospace".into(),
            ..window.text_style().font()
        };
        let font_size = window.rem_size() * 0.75;
        if self.desktop_scrollbars
            && self.measurement.as_ref().is_none_or(|cached| {
                !Arc::ptr_eq(&cached.lines, &self.visible)
                    || cached.font != font
                    || cached.size != font_size
            })
        {
            let widest = self
                .visible
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let width = window
                        .text_system()
                        .shape_line(
                            line.text.clone().into(),
                            font_size,
                            &[TextRun {
                                len: line.text.len(),
                                font: font.clone(),
                                color: cx.theme().foreground,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            }],
                            None,
                        )
                        .width;
                    (index, width)
                })
                .reduce(|a, b| if b.1 > a.1 { b } else { a })
                .map(|(index, _)| index);
            self.measurement = Some(LineMeasurement {
                lines: self.visible.clone(),
                font,
                size: font_size,
                widest,
            });
        }
        let total = self
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.lines.len());
        let path = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.stamp.as_ref())
            .map_or_else(
                || self.directory.display().to_string(),
                |stamp| stamp.path.display().to_string(),
            );
        let truncated = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.truncated);
        let message = self.error.clone().unwrap_or_else(|| {
            format!(
                "{} · {} / {total} lines{} · {path}",
                if self.paused { "Paused" } else { "Live" },
                self.visible.len(),
                if truncated {
                    " · latest 5,000 lines / 1 MiB"
                } else {
                    ""
                }
            )
        });
        let body: AnyElement = if self.visible.is_empty() {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(crate::copyable_text::copyable_text(
                    "app-logs-notice",
                    if self.error.is_some() {
                        "App logs could not be read."
                    } else if self.snapshot.is_none() {
                        "Reading app logs…"
                    } else if total == 0 {
                        "No app logs yet."
                    } else {
                        "No logs match the filters."
                    },
                ))
                .into_any_element()
        } else {
            let lines = self.visible.clone();
            uniform_list("app-log-lines", lines.len(), move |range, _, cx| {
                range
                    .filter_map(|index| lines.get(index).map(|line| (index, line)))
                    .map(|(index, line)| {
                        let color = match line.level {
                            Level::Error => cx.theme().tone(Tone::Critical),
                            Level::Warn => cx.theme().tone(Tone::Warning),
                            Level::Debug | Level::Trace => cx.theme().muted_foreground,
                            _ => cx.theme().foreground,
                        };
                        let text = line.text.clone();
                        div()
                            .id(("app-log-line", index))
                            .px_3()
                            .font_family("monospace")
                            .text_xs()
                            .text_color(color)
                            .whitespace_nowrap()
                            .child(crate::copyable_text::copyable_text(
                                "line",
                                line.text.clone(),
                            ))
                            .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
                    })
                    .collect()
            })
            .when(self.desktop_scrollbars, |list| {
                list.with_width_from_item(
                    self.measurement.as_ref().and_then(|cached| cached.widest),
                )
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .pb_4()
            })
            .track_scroll(&self.scroll)
            .size_full()
            .into_any_element()
        };

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(|_: &crate::app::CloseTab, window, cx| {
                window.defer(cx, |window, _| window.remove_window());
            })
            .child(
                TitleBar::new().child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(crate::i18n::text("Beacon — App logs")),
                ),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(crate::i18n::text("App logs")),
                    )
                    .child(
                        Button::new("pause-app-logs")
                            .small()
                            .ghost()
                            .label(if self.paused { "Resume" } else { "Pause" })
                            .tooltip(crate::i18n::text("Pause updates to read earlier lines"))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.paused = !view.paused;
                                if !view.paused {
                                    view.rebuild();
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("copy-app-logs")
                            .small()
                            .ghost()
                            .label(crate::i18n::text("Copy"))
                            .tooltip(crate::i18n::text(
                                "Copy all lines matching the current filters",
                            ))
                            .disabled(self.visible.is_empty())
                            .on_click(cx.listener(|view, _, _, cx| {
                                let text = view
                                    .visible
                                    .iter()
                                    .map(|line| line.text.as_str())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            })),
                    )
                    .child(
                        Button::new("open-app-log-folder")
                            .small()
                            .ghost()
                            .label(crate::i18n::text("Open log folder"))
                            .on_click(
                                cx.listener(|view, _, _, cx| cx.open_with_system(&view.directory)),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .px_3()
                    .pb_1()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.search).small()),
                    )
                    .children(Filter::ALL.into_iter().map(|filter| {
                        Button::new(SharedString::from(format!(
                            "app-log-filter-{}",
                            filter.label()
                        )))
                        .small()
                        .ghost()
                        .label(crate::i18n::text(filter.label()))
                        .when(self.filter == filter, |button| button.primary())
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.filter = filter;
                            view.rebuild();
                            cx.notify();
                        }))
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .child(body)
                    .when(self.desktop_scrollbars, |area| {
                        area.child(Scrollbar::horizontal(&self.scroll).mode(ScrollbarMode::Always))
                            .child(Scrollbar::vertical(&self.scroll))
                    }),
            )
            .child(
                div()
                    .id("app-log-file")
                    .px_3()
                    .py_1()
                    .text_xs()
                    .truncate()
                    .text_color(if self.error.is_some() {
                        cx.theme().tone(Tone::Critical)
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(crate::copyable_text::copyable_text(
                        "log-file-message",
                        message.clone(),
                    ))
                    .tooltip(move |window, cx| Tooltip::new(message.clone()).build(window, cx)),
            )
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;

    #[::core::prelude::v1::test]
    fn horizontal_scrollbar_reaches_the_longest_line_even_when_it_is_offscreen() {
        let cx = &mut crate::feature_test_support::context();
        let directory = tempfile::tempdir().unwrap();
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(640.), px(400.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| {
                        let mut view = AppLogs::new(directory.path().to_owned(), window, cx);
                        view.desktop_scrollbars = true;
                        view.paused = true;
                        view.visible = Arc::new(
                            (0..100)
                                .map(|index| Line {
                                    text: if index == 90 {
                                        "a long log line ".repeat(100)
                                    } else {
                                        "short".into()
                                    },
                                    level: Level::Info,
                                })
                                .collect(),
                        );
                        view
                    })
                },
            )
            .unwrap()
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).measurement.as_ref().unwrap().widest, Some(90));
            let handle = view.read(cx).scroll.0.borrow().base_handle.clone();
            let bounds = handle.bounds();
            assert!(handle.max_offset().x > px(0.));
            let y = bounds.bottom() - Scrollbar::width() / 2.;
            window.drag(
                point(bounds.left() + px(20.), y),
                point(bounds.right() - px(10.), y),
                cx,
            );
            assert!((handle.offset().x + handle.max_offset().x).abs() <= px(1.));
        })
        .unwrap();
    }
}
