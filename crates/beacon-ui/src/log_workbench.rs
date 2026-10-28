//! Shared controls and a nonmodal inspector for Pod and aggregated logs.
use crate::scroll::OverflowScrollbars as _;
use crate::{
    copyable_text::copyable_text,
    pod_logs::{Display, Filter, Level, Record},
    theme::{BeaconTheme as _, Tone},
};
use beacon_columns::Timestamp;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    select::{SearchableVec, Select, SelectEvent, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::RefCell, collections::BTreeSet, io::Write as _, rc::Rc, sync::Arc};

pub(crate) enum Changed {
    Filter,
    Range(Option<Timestamp>, Option<Timestamp>),
}
pub(crate) struct LogWorkbench {
    display: Rc<RefCell<Display>>,
    search: Entity<InputState>,
    field_filter: Entity<InputState>,
    field_search: Entity<InputState>,
    range: Entity<SelectState<SearchableVec<crate::i18n::Choice>>>,
    since: Entity<InputState>,
    until: Entity<InputState>,
    filter: Filter,
    fields_open: bool,
    time_open: bool,
    custom_time: bool,
    applied_range: Option<String>,
    error: Option<String>,
    message: Option<String>,
    exporting: bool,
    inspector: Option<Entity<LogInspector>>,
    _subscriptions: Vec<Subscription>,
    _export: Option<Task<()>>,
}
impl EventEmitter<Changed> for LogWorkbench {}
impl LogWorkbench {
    pub fn new(
        display: Rc<RefCell<Display>>,
        search: Entity<InputState>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let field_filter = cx.new(|cx| {
                crate::i18n::input("Exact fields: status=503 service=\"api server\"", window, cx)
            });
            let field_search = cx.new(|cx| crate::i18n::input("Find a field", window, cx));
            let range = cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(
                        [
                            "Latest 2,000 lines",
                            "Last 5 minutes",
                            "Last 15 minutes",
                            "Last hour",
                            "Last 24 hours",
                            "Custom range",
                        ]
                        .map(crate::i18n::Choice::label)
                        .to_vec(),
                    ),
                    Some(gpui_kit::component::IndexPath::default()),
                    window,
                    cx,
                )
                .searchable(true)
            });
            let since = cx.new(|cx| {
                crate::i18n::input("From · RFC3339, e.g. 2026-10-13T10:00:00Z", window, cx)
            });
            let until =
                cx.new(|cx| crate::i18n::input("Until · RFC3339 (optional)", window, cx));
            let subscriptions = vec![
                cx.subscribe(
                    &field_filter,
                    |view: &mut Self, _, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            view.error = None;
                            let value = view.field_filter.read(cx).value();
                            match if value.trim().is_empty() {
                                Ok(Default::default())
                            } else {
                                crate::pod_logs::logfmt(&value)
                            } {
                                Ok(predicates) => {
                                    view.filter.predicates = predicates;
                                    cx.emit(Changed::Filter);
                                }
                                Err(error) => view.error = Some(error),
                            };
                            cx.notify();
                        }
                    },
                ),
                cx.subscribe(&field_search, |_: &mut Self, _, _: &InputEvent, cx| {
                    cx.notify()
                }),
                cx.subscribe_in(
                    &range,
                    window,
                    |view: &mut Self, _, event: &SelectEvent<SearchableVec<crate::i18n::Choice>>, _, cx| {
                        if let SelectEvent::Confirm(Some(value)) = event {
                            view.custom_time = value == "Custom range";
                            view.time_open = view.custom_time;
                            view.error = None;
                            if !view.custom_time {
                                let seconds = match value.as_str() {
                                    "Last 5 minutes" => Some(300),
                                    "Last 15 minutes" => Some(900),
                                    "Last hour" => Some(3600),
                                    "Last 24 hours" => Some(86400),
                                    _ => None,
                                };
                                let since = seconds.map(|seconds| {
                                    Timestamp::from_second(Timestamp::now().as_second() - seconds)
                                        .unwrap()
                                });
                                view.applied_range = since.map(|time| crate::i18n::message("messages.since_time_server_timestamps_available_container_logs", &[("time", (time).to_string())]).to_string());
                                cx.emit(Changed::Range(since, None));
                            }
                            cx.notify();
                        }
                    },
                ),
            ];
            Self {
                display,
                search,
                field_filter,
                field_search,
                range,
                since,
                until,
                filter: Filter::default(),
                fields_open: false,
                time_open: false,
                custom_time: false,
                applied_range: None,
                error: None,
                message: None,
                exporting: false,
                inspector: None,
                _subscriptions: subscriptions,
                _export: None,
            }
        })
    }
    pub fn filter(&self, cx: &App) -> Filter {
        let mut filter = self.filter.clone();
        filter.query = self.search.read(cx).value().to_string();
        filter
    }
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }
    pub fn inspect(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(record) = self.display.borrow().record(id) else {
            return;
        };
        let workbench = cx.entity().downgrade();
        self.inspector = Some(cx.new(|_| LogInspector {
            id,
            record,
            workbench,
        }));
        cx.emit(Changed::Filter);
        cx.notify();
        let _ = window;
    }
    pub fn inspector(&self) -> Option<Entity<LogInspector>> {
        self.inspector.clone()
    }
    pub fn bookmark(&mut self, id: usize, cx: &mut Context<Self>) {
        self.display.borrow_mut().toggle_bookmark(id);
        cx.emit(Changed::Filter);
        cx.notify();
    }
    fn apply_range(&mut self, cx: &mut Context<Self>) {
        let parse = |input: &Entity<InputState>| -> Result<Option<Timestamp>, String> {
            let value = input.read(cx).value();
            if value.trim().is_empty() {
                Ok(None)
            } else {
                value.trim().parse().map(Some).map_err(|_| {
                    "Use an RFC3339 time with a timezone, e.g. 2026-10-13T10:00:00Z.".into()
                })
            }
        };
        match parse(&self.since).and_then(|since| parse(&self.until).map(|until| (since, until))) {
            Ok((since, until)) if since.zip(until).is_some_and(|(since, until)| since > until) => {
                self.error = Some("From must be before Until.".into())
            }
            Ok((since, until)) => {
                self.error = None;
                self.applied_range = Some(
                    crate::i18n::message(
                        "messages.server_timestamps_available_container_logs",
                        &[
                            (
                                "value0",
                                (since
                                    .map(|time| time.to_string())
                                    .unwrap_or_else(|| "Container start".into()))
                                .to_string(),
                            ),
                            (
                                "value1",
                                (until
                                    .map(|time| time.to_string())
                                    .unwrap_or_else(|| "Live".into()))
                                .to_string(),
                            ),
                        ],
                    )
                    .to_string(),
                );
                cx.emit(Changed::Range(since, until));
                self.time_open = false;
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
    fn export(&mut self, cx: &mut Context<Self>) {
        if self.exporting {
            return;
        }
        let text = self.display.borrow().text();
        let count = self.display.borrow().rows.len();
        let directory = directories::UserDirs::new()
            .map(|dirs| dirs.download_dir().unwrap_or(dirs.home_dir()).to_owned())
            .unwrap_or_else(std::env::temp_dir);
        let prompt = cx.prompt_for_new_path(&directory, Some("beacon-filtered-logs.log"));
        self.exporting = true;
        self.message = Some(
            crate::i18n::message(
                "messages.choose_where_to_export_count_matching_retained_lines",
                &[("count", (count).to_string())],
            )
            .to_string(),
        );
        self._export = Some(cx.spawn(async move |view, cx| {
            let result = match prompt.await {
                Ok(Ok(Some(path))) => {
                    cx.background_executor()
                        .spawn(async move {
                            let folder = path.parent().unwrap_or_else(|| std::path::Path::new("."));
                            let mut temporary = tempfile::NamedTempFile::new_in(folder)
                                .map_err(|error| error.to_string())?;
                            temporary
                                .write_all(text.as_bytes())
                                .map_err(|error| error.to_string())?;
                            temporary
                                .as_file_mut()
                                .sync_all()
                                .map_err(|error| error.to_string())?;
                            temporary
                                .persist(&path)
                                .map_err(|error| error.to_string())?;
                            Ok(crate::i18n::message(
                                "messages.exported_count_matching_retained_lines_to",
                                &[
                                    ("count", (count).to_string()),
                                    ("value0", (path.display()).to_string()),
                                ],
                            )
                            .to_string())
                        })
                        .await
                }
                Ok(Ok(None)) => Ok("Export cancelled.".into()),
                other => Err(crate::i18n::message(
                    "messages.could_not_choose_an_export_file_other",
                    &[("other", format!("{:?}", other))],
                )
                .to_string()),
            };
            let _ = view.update(cx, |view, cx| {
                view.exporting = false;
                view.message = Some(result.unwrap_or_else(|error: String| error));
                cx.notify();
            });
        }));
        cx.notify();
    }
}
impl Render for LogWorkbench {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fields = self
            .display
            .borrow()
            .fields
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let error = self
            .error
            .clone()
            .or_else(|| self.display.borrow().error.clone());
        let bookmark_count = self.display.borrow().bookmarks.len();
        v_flex().w_full().flex_shrink_0().border_b_1().border_color(cx.theme().border).bg(cx.theme().background).text_xs()
            .child(h_flex().id("structured-log-toolbar").w_full().min_w_0().overflow_x_scrollbar().whitespace_nowrap().gap_1().px_3().py_1()
                .child(Button::new("log-regex").xsmall().ghost().selected(self.filter.regex).label(".*").tooltip(crate::i18n::text("Regular expression search (case insensitive)")).on_click(cx.listener(|view, _, _, cx| { view.filter.regex = !view.filter.regex; cx.emit(Changed::Filter); cx.notify(); })))
                .children(Level::ALL.map(|level| { let label = level.label(); Button::new(format!("log-level-{label}")).xsmall().ghost().selected(self.filter.levels.contains(label)).label(label).on_click(cx.listener(move |view, _, _, cx| { if !view.filter.levels.remove(label) { view.filter.levels.insert(label.into()); } cx.emit(Changed::Filter); cx.notify(); })) }))
                .child(div().h(px(16.)).border_l_1().border_color(cx.theme().border).mx_1())
                .child(Button::new("log-fields").xsmall().ghost().selected(self.fields_open).label(crate::i18n::message("messages.fields", &[("value0", (if self.filter.predicates.is_empty() { String::new() } else { format!(" {}", self.filter.predicates.len()) }).to_string())]).to_string()).tooltip(crate::i18n::text("Filter exact field values and choose displayed fields")).on_click(cx.listener(|view, _, _, cx| { view.fields_open = !view.fields_open; cx.notify(); })))
                .child(Button::new("log-compact").xsmall().ghost().selected(self.filter.compact).label(crate::i18n::text("Compact")).on_click(cx.listener(|view, _, _, cx| { view.filter.compact = !view.filter.compact; cx.emit(Changed::Filter); cx.notify(); })))
                .child(div().w(px(170.)).flex_shrink_0().child(Select::new(&self.range).id("log-time-range").accessibility_label(crate::i18n::text("Log time range")).search_placeholder(crate::i18n::text("Search time ranges")).xsmall().w_full()))
                .when(self.custom_time, |bar| bar.child(Button::new("log-edit-range").xsmall().ghost().label(crate::i18n::text("Edit range")).on_click(cx.listener(|view, _, _, cx| { view.time_open = !view.time_open; cx.notify(); }))))
                .child(Button::new("log-bookmarks").xsmall().ghost().selected(self.filter.bookmarks).label(crate::i18n::message("messages.bookmarks_bookmark_count", &[("bookmark_count", (bookmark_count).to_string())]).to_string()).tooltip(crate::i18n::text("Saved lines survive buffer eviction and container reloads in this panel")).on_click(cx.listener(|view, _, _, cx| { view.filter.bookmarks = !view.filter.bookmarks; cx.emit(Changed::Filter); cx.notify(); })))
                .when(!self.filter.levels.is_empty() || !self.filter.predicates.is_empty() || self.filter.regex || self.filter.bookmarks, |bar| bar.child(Button::new("log-reset-filters").xsmall().ghost().label(crate::i18n::text("Reset filters")).on_click(cx.listener(|view, _, window, cx| { view.filter.levels.clear(); view.filter.predicates.clear(); view.filter.regex = false; view.filter.bookmarks = false; view.error = None; view.field_filter.update(cx, |input, cx| input.set_value("", window, cx)); view.search.update(cx, |input, cx| input.set_value("", window, cx)); cx.emit(Changed::Filter); cx.notify(); }))))
                .child(div().flex_1())
                .child(Button::new("log-export-filtered").xsmall().ghost().label(crate::i18n::text("Export filtered…")).disabled(self.exporting || error.is_some()).tooltip(crate::i18n::text("Export matching retained lines as raw text; includes current level, search, fields and bookmark filters")).on_click(cx.listener(|view, _, _, cx| view.export(cx)))))
            .when(self.fields_open, |panel| panel.child(v_flex().id("log-fields-panel").test_support().gap_2().px_3().py_2().bg(cx.theme().muted.opacity(0.35))
                .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.field_filter).id("log-field-filter").xsmall())).child(Button::new("log-clear-field-filter").xsmall().ghost().label(crate::i18n::text("Clear")).on_click(cx.listener(|view, _, window, cx| { view.field_filter.update(cx, |input, cx| input.set_value("", window, cx)); }))))
                .child(h_flex().gap_2().items_center().child(crate::i18n::text("Display fields")).child(div().w(px(180.)).child(Input::new(&self.field_search).id("log-field-search").xsmall())).child(Button::new("log-all-fields").xsmall().ghost().label(crate::i18n::text("All")).on_click(cx.listener(|view, _, _, cx| { view.filter.fields = None; view.filter.compact = true; cx.emit(Changed::Filter); cx.notify(); }))).child(Button::new("log-no-fields").xsmall().ghost().label(crate::i18n::text("None")).on_click(cx.listener(|view, _, _, cx| { view.filter.fields = Some(BTreeSet::new()); view.filter.compact = true; cx.emit(Changed::Filter); cx.notify(); }))))
                .child(h_flex().id("log-field-options").flex_wrap().gap_1().max_h(px(120.)).overflow_y_scrollbar().children(fields.into_iter().filter(|field| field.to_lowercase().contains(&self.field_search.read(cx).value().to_lowercase())).take(200).enumerate().map(|(index, field)| {
                    let selected = self.filter.fields.as_ref().is_none_or(|fields| fields.contains(&field));
                    Button::new(("log-show-field", index)).xsmall().ghost().selected(selected).label(field.clone()).on_click(cx.listener(move |view, _, _, cx| { let fields = view.filter.fields.get_or_insert_with(|| view.display.borrow().fields.keys().cloned().collect::<BTreeSet<_>>()); if !fields.remove(&field) { fields.insert(field.clone()); } view.filter.compact = true; cx.emit(Changed::Filter); cx.notify(); }))
                })))
                .when(self.display.borrow().fields.is_empty(), |panel| panel.child(crate::i18n::text("Fields appear as JSON or logfmt lines arrive.")))
                .when(self.display.borrow().fields.len() > 200, |panel| panel.child(crate::i18n::text("Showing up to 200 fields. Search to find other fields.")))))
            .when(self.time_open && self.custom_time, |panel| panel.child(h_flex().id("log-custom-range").gap_2().px_3().py_2().items_end().child(v_flex().flex_1().gap_1().child(crate::i18n::text("From · server time")).child(Input::new(&self.since).id("log-since").xsmall())).child(v_flex().flex_1().gap_1().child(crate::i18n::text("Until · optional, snapshot")).child(Input::new(&self.until).id("log-until").xsmall())).child(Button::new("log-apply-range").xsmall().primary().label(crate::i18n::text("Query range")).on_click(cx.listener(|view, _, _, cx| view.apply_range(cx))))))
            .children(error.map(|error| div().id("log-filter-error").test_support().px_3().py_1().text_color(cx.theme().danger).child(copyable_text("log-filter-error-text", error))))
            .children(self.applied_range.as_ref().map(|range| div().px_3().py_1().text_color(cx.theme().muted_foreground).child(copyable_text("log-applied-range", range.clone()))))
            .children(self.message.as_ref().map(|message| div().px_3().py_1().text_color(cx.theme().muted_foreground).child(copyable_text("log-export-status", message.clone()))))
    }
}

pub(crate) struct LogInspector {
    id: usize,
    record: Arc<Record>,
    workbench: WeakEntity<LogWorkbench>,
}
impl Render for LogInspector {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workbench = self.workbench.clone();
        let bookmark = self.workbench.clone();
        let id = self.id;
        v_flex()
            .id("log-inspector")
            .test_support()
            .w_full()
            .h(px(250.))
            .min_h(px(130.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(crate::i18n::text("Log entry")),
                    )
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.record.format.label()),
                    )
                    .child(
                        div()
                            .text_color(cx.theme().tone(match self.record.level {
                                Level::Error => Tone::Critical,
                                Level::Warning => Tone::Warning,
                                Level::Info => Tone::Healthy,
                                _ => Tone::Unknown,
                            }))
                            .child(self.record.level.label()),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("log-inspector-bookmark")
                            .xsmall()
                            .ghost()
                            .label(crate::i18n::text("Toggle bookmark"))
                            .on_click(move |_, _, cx| {
                                let _ = bookmark.update(cx, |view, cx| view.bookmark(id, cx));
                            }),
                    )
                    .child(
                        Button::new("log-inspector-close")
                            .xsmall()
                            .ghost()
                            .label(crate::i18n::text("Close"))
                            .on_click(move |_, _, cx| {
                                let _ = workbench.update(cx, |view, cx| {
                                    view.inspector = None;
                                    cx.emit(Changed::Filter);
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .child(
                h_flex()
                    .size_full()
                    .min_size_0()
                    .gap_3()
                    .px_3()
                    .pb_2()
                    .child(
                        v_flex()
                            .id("log-inspector-fields")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scrollbar()
                            .gap_1()
                            .children(self.record.fields.iter().enumerate().map(
                                |(index, (field, value))| {
                                    let workbench = self.workbench.clone();
                                    let field = field.clone();
                                    let value = value.clone();
                                    h_flex()
                                        .items_start()
                                        .gap_2()
                                        .child(
                                            div()
                                                .w(px(140.))
                                                .flex_shrink_0()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(copyable_text(
                                                    ("log-field-key", index),
                                                    field.clone(),
                                                )),
                                        )
                                        .child(div().flex_1().min_w_0().child(copyable_text(
                                            ("log-field-value", index),
                                            value.clone(),
                                        )))
                                        .child(
                                            Button::new(("log-filter-value", index))
                                                .xsmall()
                                                .ghost()
                                                .label(crate::i18n::text("Filter"))
                                                .tooltip(crate::i18n::text(
                                                    "Add this exact field value to the filter",
                                                ))
                                                .on_click(move |_, window, cx| {
                                                    let _ = workbench.update(cx, |view, cx| {
                                                        view.filter
                                                            .predicates
                                                            .insert(field.clone(), value.clone());
                                                        let text = crate::pod_logs::predicate_text(
                                                            &view.filter.predicates,
                                                        );
                                                        view.field_filter.update(
                                                            cx,
                                                            |input, cx| {
                                                                input.set_value(text, window, cx)
                                                            },
                                                        );
                                                        view.fields_open = true;
                                                        cx.emit(Changed::Filter);
                                                        cx.notify();
                                                    });
                                                }),
                                        )
                                },
                            ))
                            .when(self.record.fields.is_empty(), |view| {
                                view.child(crate::i18n::text("No structured fields in this line."))
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(crate::i18n::text("Raw log · unchanged")),
                            )
                            .child(
                                div()
                                    .id("log-inspector-raw")
                                    .flex_1()
                                    .min_size_0()
                                    .overflow_y_scrollbar()
                                    .font_family("monospace")
                                    .child(copyable_text("log-raw-text", self.record.raw.clone())),
                            ),
                    ),
            )
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::{
        feature_test_support as support,
        pod_tools::{LogCanvas, LogLineWidths},
    };
    use beacon_kube::LogBuffer;
    use gpui_kit::test::TestWindowExt as _;
    struct Pane {
        logs: LogBuffer,
        display: Rc<RefCell<Display>>,
        controls: Entity<LogWorkbench>,
        wrapped: ListState,
        scroll: UniformListScrollHandle,
        widths: LogLineWidths,
        requested: Option<(Option<Timestamp>, Option<Timestamp>)>,
        _subscription: Subscription,
    }
    impl Pane {
        fn refresh(&mut self, cx: &mut Context<Self>) {
            self.display
                .borrow_mut()
                .update_with(&self.logs, &self.controls.read(cx).filter(cx));
            self.wrapped.reset(self.display.borrow().rows.len());
            self.controls.update(cx, |view, cx| view.refresh(cx));
            cx.notify();
        }
    }
    impl Render for Pane {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let canvas = LogCanvas {
                workbench: Some(self.controls.clone()),
                display: self.display.clone(),
                wrap: false,
                wrapped: &self.wrapped,
                unwrapped: &self.scroll,
                widths: &mut self.widths,
            }
            .render(window, cx);
            v_flex()
                .w(px(1300.))
                .h(px(850.))
                .child(
                    Input::new(&self.controls.read(cx).search)
                        .id("test-log-search")
                        .xsmall(),
                )
                .child(self.controls.clone())
                .child(div().flex_1().min_size_0().child(canvas))
                .children(self.controls.read(cx).inspector())
        }
    }
    fn pane(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Pane>) {
        let (window, pane) = cx.update(|cx| gpui_kit::open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(1400.), px(900.))))), ..Default::default() }, cx, |window, cx| {
            window.set_view_retention(false);
            let display: Rc<RefCell<Display>> = Rc::default();
            let search = cx.new(|cx| InputState::new(window, cx));
            let controls = LogWorkbench::new(display.clone(), search, window, cx);
            cx.new(|cx| {
                let subscription = cx.subscribe(&controls, |view: &mut Pane, _, event: &Changed, cx| { match event { Changed::Filter => view.refresh(cx), Changed::Range(since, until) => { view.requested = Some((*since, *until)); cx.notify(); } } });
                let mut logs = LogBuffer::new(); logs.extend(["2026-10-13T10:00:00Z {\"level\":\"error\",\"msg\":\"request failed\",\"status\":503}", "2026-10-13T10:00:01Z level=info msg=ready status=200", "plain text"].map(str::to_string));
                display.borrow_mut().update_with(&logs, &Filter::default());
                Pane { logs, display, controls, wrapped: ListState::new(3, ListAlignment::Top, px(300.)), scroll: UniformListScrollHandle::new(), widths: LogLineWidths::default(), requested: None, _subscription: subscription }
            })
        }).unwrap());
        (window, pane)
    }
    #[::core::prelude::v1::test]
    fn inspect_filter_fields_bookmark_and_export_raw_matching_lines() {
        let cx = &mut support::context();
        let (window, pane) = pane(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("inspect-log", 0usize), cx);
            window.render_frame(cx);
            assert!(window.find("log-inspector").visible());
            window.click(("log-filter-value", 2usize), cx);
            window.render_frame(cx); // status=503
        })
        .unwrap();
        assert_eq!(
            pane.read_with(cx, |view, _| view.display.borrow().rows.len()),
            1
        );
        cx.update_window(window, |_, window, cx| {
            window.click("log-inspector-bookmark", cx);
            window.click("log-inspector-close", cx);
            window.render_frame(cx);
            window.click("log-compact", cx);
            window.render_frame(cx);
            assert!(window.find("log-fields-panel").visible());
            window.click(("log-show-field", 2usize), cx); // hide status in compact mode
            window.click("log-export-filtered", cx);
        })
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("filtered.log");
        let destination = path.clone();
        cx.simulate_new_path_selection(move |_| Some(destination));
        support::settle(cx, |cx| {
            pane.read_with(cx, |view, cx| !view.controls.read(cx).exporting)
        });
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("\"status\":503"));
        assert!(!text.contains("ready"));
        pane.update(cx, |view, cx| {
            view.logs.clear();
            view.logs.push("different stream".into());
            view.display.borrow_mut().reset_stream();
            view.refresh(cx);
        });
        cx.update_window(window, |_, window, cx| {
            window.click("log-clear-field-filter", cx);
            window.click("log-bookmarks", cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(("inspect-log", 0usize)).visible());
        })
        .unwrap();
        assert_eq!(
            pane.read_with(cx, |view, _| view.display.borrow().bookmarks.len()),
            1
        );
    }
    #[::core::prelude::v1::test]
    fn level_regex_and_time_controls_validate_and_emit_real_queries() {
        let cx = &mut support::context();
        let (window, pane) = pane(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("log-level-Error", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(
            pane.read_with(cx, |view, _| view.display.borrow().rows.len()),
            1
        );
        cx.update_window(window, |_, window, cx| {
            let controls = pane.read(cx).controls.clone();
            controls.update(cx, |view, cx| {
                view.search
                    .update(cx, |input, cx| input.set_value("[", window, cx));
            });
            window.click("log-regex", cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("log-filter-error").visible());
            let controls = pane.read(cx).controls.clone();
            controls.update(cx, |view, cx| {
                view.custom_time = true;
                view.time_open = true;
                view.since
                    .update(cx, |input, cx| input.set_value("not-a-time", window, cx));
                cx.notify();
            });
            window.render_frame(cx);
            window.click("log-apply-range", cx);
            window.render_frame(cx);
            assert!(window.find("log-filter-error").visible());
            controls.update(cx, |view, cx| {
                view.since.update(cx, |input, cx| {
                    input.set_value("2026-10-13T09:00:00Z", window, cx)
                });
                view.until.update(cx, |input, cx| {
                    input.set_value("2026-10-13T11:00:00Z", window, cx)
                });
            });
            window.click("log-apply-range", cx);
        })
        .unwrap();
        let requested = pane.read_with(cx, |view, _| view.requested.unwrap());
        assert_eq!(requested.0.unwrap().to_string(), "2026-10-13T09:00:00Z");
        assert_eq!(requested.1.unwrap().to_string(), "2026-10-13T11:00:00Z");
    }
}
