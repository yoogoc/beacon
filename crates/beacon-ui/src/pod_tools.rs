//! Pod logs, commands and interactive shells in the independent bottom panel.

use crate::scroll::OverflowScrollbars as _;
#[cfg(test)]
use std::collections::VecDeque;
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

use beacon_kube::{
    ClusterSession, DynamicObject, LogBuffer, LogEvent, LogOptions, ObjectRef, Rules,
};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectGroup, SelectState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::bridge::{Bridge, drain_into};
use crate::theme::{BeaconTheme as _, Tone};
mod workspace_controls;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PodToolTab {
    Logs,
    Exec,
    Shell,
}

impl PodToolTab {
    pub(crate) const ALL: [Self; 3] = [Self::Logs, Self::Exec, Self::Shell];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Logs => "Logs",
            Self::Exec => "Exec",
            Self::Shell => "Shell",
        }
    }
}

pub(crate) struct PodToolsView {
    session: Arc<ClusterSession>,
    target: ObjectRef,
    object: Arc<DynamicObject>,
    rules: Option<Arc<Rules>>,
    tab: PodToolTab,
    /// Standalone views retain the original header; workspaces own session tabs.
    embedded: Option<bool>,
    log_advanced: bool,
    command: Entity<InputState>,
    ran: Exec,
    shell: Option<Entity<crate::terminal::TerminalView>>,
    logs: LogBuffer,
    log_options: LogOptions,
    log_status: LogStatus,
    log_scroll: UniformListScrollHandle,
    log_widths: LogLineWidths,
    log_search: Entity<InputState>,
    log_workbench: Entity<crate::log_workbench::LogWorkbench>,
    log_display: Rc<RefCell<crate::pod_logs::Display>>,
    log_wrap_scroll: ListState,
    log_wrap: bool,
    log_follow: bool,
    container_groups: Vec<ContainerGroup>,
    container_picker: Entity<SelectState<ContainerChoices>>,
    download: Download,
    download_directory: Option<PathBuf>,
    download_abort: Option<tokio::task::AbortHandle>,
    _logs_task: Option<Task<()>>,
    _download_task: Option<Task<()>>,
    _exec_task: Option<Task<()>>,
    _container_subscription: Subscription,
    _search_subscription: Subscription,
    _workbench_subscription: Subscription,
}

pub(crate) struct PodToolsClosed;
impl EventEmitter<PodToolsClosed> for PodToolsView {}

/// What the Exec tab has to show.
enum Exec {
    Idle,
    Running,
    Done(Box<beacon_kube::Output>),
    Failed(String),
}

/// What the Logs tab is doing.
#[derive(Debug, PartialEq, Eq)]
enum LogStatus {
    Unopened,
    Waiting,
    Following,
    /// The stream ended, which for a followed log means the container did.
    Ended,
    Failed(String),
}

enum Download {
    Idle,
    Choosing,
    Running(String),
    Cancelling,
    Saved { path: PathBuf, bytes: u64 },
    Cancelled,
    Failed(String),
}

type ContainerChoices = SearchableVec<SelectGroup<String>>;

#[derive(Clone, PartialEq, Eq)]
struct ContainerGroup {
    title: &'static str,
    names: Vec<String>,
}

fn container_groups(object: &DynamicObject) -> Vec<ContainerGroup> {
    [
        ("containers", "Containers"),
        ("initContainers", "Init containers"),
        ("ephemeralContainers", "Ephemeral containers"),
    ]
    .into_iter()
    .filter_map(|(field, title)| {
        let names: Vec<_> = object
            .data
            .get("spec")?
            .get(field)?
            .as_array()?
            .iter()
            .filter_map(|container| container.get("name")?.as_str().map(str::to_owned))
            .collect();
        (!names.is_empty()).then_some(ContainerGroup { title, names })
    })
    .collect()
}

fn default_container(object: &DynamicObject, groups: &[ContainerGroup]) -> Option<String> {
    let normal = groups.iter().find(|group| group.title == "Containers");
    object
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("kubectl.kubernetes.io/default-container"))
        .filter(|name| normal.is_some_and(|group| group.names.contains(name)))
        .cloned()
        .or_else(|| groups.first()?.names.first().cloned())
}

fn container_choices(groups: &[ContainerGroup]) -> ContainerChoices {
    SearchableVec::new(
        groups
            .iter()
            .map(|group| {
                SelectGroup::new(crate::i18n::text(group.title)).items(group.names.clone())
            })
            .collect::<Vec<_>>(),
    )
}

fn log_filename(
    target: &ObjectRef,
    options: &LogOptions,
    now: beacon_columns::Timestamp,
) -> String {
    fn part(value: &str, limit: usize) -> String {
        value
            .chars()
            .take(limit)
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                    ch
                } else {
                    '_'
                }
            })
            .collect()
    }
    format!(
        "pod_{}_{}_{}_{}{}.log",
        part(target.namespace.as_deref().unwrap_or("default"), 32),
        part(&target.name, 80),
        part(options.container.as_deref().unwrap_or("default"), 48),
        now.strftime("%Y%m%d-%H%M%S-UTC"),
        if options.previous { "_previous" } else { "" }
    )
}

/// Keep measured widths aligned with the bounded log buffer. Only arriving
/// lines need shaping; scrolling through history reuses their measurements.
#[derive(Default)]
pub(crate) struct LogLineWidths {
    wrapped_font: Option<(Font, Pixels)>,
    font: Option<(Font, Pixels)>,
    #[cfg(test)]
    dropped: usize,
    #[cfg(test)]
    widths: VecDeque<Pixels>,
    projected: std::collections::BTreeMap<usize, (SharedString, Pixels)>,
}

fn render_log_row(
    row: &crate::pod_logs::Row,
    wrap: bool,
    workbench: Option<Entity<crate::log_workbench::LogWorkbench>>,
    cx: &App,
) -> AnyElement {
    let highlights = row
        .matches
        .iter()
        .cloned()
        .map(|range| {
            (
                range,
                HighlightStyle {
                    background_color: Some(cx.theme().warning.opacity(0.3)),
                    ..Default::default()
                },
            )
        })
        .collect();
    let id = row.id;
    h_flex()
        .id(("pod-log-line", row.id))
        .gap_1()
        .items_start()
        .px_3()
        .min_h(px(18.))
        .font_family("monospace")
        .text_xs()
        .when(wrap, |row| row.w_full())
        .when(!wrap, |row| row.whitespace_nowrap())
        .when_some(workbench, |line, workbench| {
            let inspect = workbench.clone();
            line.child(
                Button::new(("inspect-log", id))
                    .xsmall()
                    .ghost()
                    .label("›")
                    .tooltip(crate::i18n::text("Inspect fields and raw log"))
                    .on_click(move |_, window, cx| {
                        inspect.update(cx, |view, cx| view.inspect(id, window, cx))
                    }),
            )
            .child(
                Button::new(("bookmark-log", id))
                    .xsmall()
                    .ghost()
                    .label(if row.bookmarked { "★" } else { "☆" })
                    .tooltip(crate::i18n::text("Bookmark this log line"))
                    .on_click(move |_, _, cx| {
                        workbench.update(cx, |view, cx| view.bookmark(id, cx))
                    }),
            )
            .child(
                div()
                    .w(px(52.))
                    .flex_shrink_0()
                    .text_color(cx.theme().tone(match row.record.level {
                        crate::pod_logs::Level::Error => Tone::Critical,
                        crate::pod_logs::Level::Warning => Tone::Warning,
                        _ => Tone::Unknown,
                    }))
                    .child(row.record.level.label()),
            )
        })
        .child(div().when(wrap, |text| text.flex_1().min_w_0()).child(
            crate::copyable_text::highlighted_text("line", row.text.clone(), highlights),
        ))
        .test_support()
        .into_any_element()
}

pub(crate) struct LogCanvas<'a> {
    pub(crate) workbench: Option<Entity<crate::log_workbench::LogWorkbench>>,
    pub(crate) display: Rc<RefCell<crate::pod_logs::Display>>,
    pub(crate) wrap: bool,
    pub(crate) wrapped: &'a ListState,
    pub(crate) unwrapped: &'a UniformListScrollHandle,
    pub(crate) widths: &'a mut LogLineWidths,
}

impl LogCanvas<'_> {
    pub(crate) fn render(&mut self, window: &mut Window, cx: &App) -> AnyElement {
        let count = self.display.borrow().rows.len();
        let wrapped_font = (window.text_style().font(), window.rem_size());
        if self.widths.wrapped_font.as_ref() != Some(&wrapped_font) {
            self.wrapped.remeasure();
            self.widths.wrapped_font = Some(wrapped_font);
        }
        let display = self.display.clone();
        let workbench = self.workbench.clone();
        if self.wrap {
            div()
                .relative()
                .size_full()
                .min_w_0()
                .overflow_hidden()
                .child(
                    list(self.wrapped.clone(), move |index, _, cx| {
                        display
                            .borrow()
                            .rows
                            .get(index)
                            .map(|row| render_log_row(row, true, workbench.clone(), cx))
                            .unwrap_or_else(|| div().into_any_element())
                    })
                    .size_full()
                    .pr_3(),
                )
                .child(Scrollbar::vertical(self.wrapped))
                .into_any_element()
        } else {
            let font = Font {
                family: "monospace".into(),
                ..window.text_style().font()
            };
            let font_size = window.rem_size() * 0.75;
            let widest = self.widths.widest_display(
                &self.display.borrow(),
                font.clone(),
                font_size,
                |line| {
                    window
                        .text_system()
                        .shape_line(
                            line.to_owned().into(),
                            font_size,
                            &[TextRun {
                                len: line.len(),
                                font: font.clone(),
                                color: cx.theme().foreground,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            }],
                            None,
                        )
                        .width
                },
            );
            let list = uniform_list("log-lines", count, move |range, _, cx| {
                let display = display.borrow();
                range
                    .filter_map(|index| display.rows.get(index))
                    .map(|row| render_log_row(row, false, workbench.clone(), cx))
                    .collect()
            })
            .with_width_from_item(widest)
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .track_scroll(self.unwrapped)
            .size_full()
            .pb_3();
            div()
                .relative()
                .size_full()
                .min_w_0()
                .overflow_hidden()
                .child(list)
                .child(Scrollbar::horizontal(self.unwrapped).mode(ScrollbarMode::Always))
                .child(Scrollbar::vertical(self.unwrapped))
                .into_any_element()
        }
    }
}

impl LogLineWidths {
    fn widest_display(
        &mut self,
        display: &crate::pod_logs::Display,
        font: Font,
        size: Pixels,
        mut measure: impl FnMut(&str) -> Pixels,
    ) -> Option<usize> {
        let font = (font, size);
        if self.font.as_ref() != Some(&font) {
            self.projected.clear();
            self.font = Some(font);
        }
        let ids = display
            .rows
            .iter()
            .map(|row| row.id)
            .collect::<std::collections::BTreeSet<_>>();
        self.projected.retain(|id, _| ids.contains(id));
        display
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let cached = self
                    .projected
                    .entry(row.id)
                    .or_insert_with(|| (row.text.clone(), measure(&row.text)));
                if cached.0 != row.text {
                    *cached = (row.text.clone(), measure(&row.text));
                }
                (index, cached.1)
            })
            .reduce(|a, b| if b.1 > a.1 { b } else { a })
            .map(|(index, _)| index)
    }
    #[cfg(test)]
    fn widest(
        &mut self,
        logs: &LogBuffer,
        font: Font,
        font_size: Pixels,
        mut measure: impl FnMut(&str) -> Pixels,
    ) -> Option<usize> {
        let font = (font, font_size);
        if self.font.as_ref() != Some(&font) || logs.dropped() < self.dropped {
            self.widths.clear();
            self.font = Some(font);
        }
        let removed = logs.dropped().saturating_sub(self.dropped);
        self.widths.drain(..removed.min(self.widths.len()));
        self.dropped = logs.dropped();
        self.widths.truncate(logs.len());
        self.widths
            .extend(logs.lines().skip(self.widths.len()).map(&mut measure));
        self.widths
            .iter()
            .enumerate()
            .reduce(|widest, line| if line.1 > widest.1 { line } else { widest })
            .map(|(index, _)| index)
    }
}

impl PodToolsView {
    pub(crate) fn has_active_session(&self, cx: &App) -> bool {
        matches!(self.ran, Exec::Running)
            || self
                .shell
                .as_ref()
                .is_some_and(|shell| shell.read(cx).is_active())
    }

    pub(crate) fn new(
        session: Arc<ClusterSession>,
        object: Arc<DynamicObject>,
        rules: Option<Arc<Rules>>,
        tab: PodToolTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let command = cx.new(|cx| {
            crate::i18n::input("A command to run, e.g. ls -la /etc", window, cx)
                .default_value("ls -la /")
        });
        let container_groups = container_groups(&object);
        let container = default_container(&object, &container_groups);
        let container_picker = cx.new(|cx| {
            let mut picker =
                SelectState::new(container_choices(&container_groups), None, window, cx)
                    .searchable(true);
            if let Some(container) = &container {
                picker.set_selected_value(container, window, cx);
            }
            picker
        });
        let subscription = cx.subscribe_in(
            &container_picker,
            window,
            |view, _, event: &SelectEvent<ContainerChoices>, window, cx| {
                if let SelectEvent::Confirm(Some(container)) = event {
                    view.set_log_options(
                        LogOptions {
                            container: Some(container.clone()),
                            ..view.log_options.clone()
                        },
                        window,
                        cx,
                    );
                }
            },
        );
        let log_search =
            cx.new(|cx| crate::i18n::input("Search logs", window, cx).clean_on_escape());
        let search_subscription = cx.subscribe_in(&log_search, window, |view, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                view.update_log_display(cx);
                view.set_log_follow(false);
                cx.notify();
            }
        });
        cx.observe_global_in::<crate::i18n::LocaleState>(window, |view, _, cx| {
            // The next render refreshes translated group headings, keeping the
            // container value and the existing log stream unchanged.
            view.container_groups.clear();
            cx.notify();
        })
        .detach();
        let log_display: Rc<RefCell<crate::pod_logs::Display>> = Rc::default();
        let log_workbench = crate::log_workbench::LogWorkbench::new(
            log_display.clone(),
            log_search.clone(),
            window,
            cx,
        );
        let workbench_subscription =
            cx.subscribe_in(&log_workbench, window, |view, _, event, window, cx| {
                match event {
                    crate::log_workbench::Changed::Filter => {
                        view.update_log_display(cx);
                        view.set_log_follow(false);
                    }
                    crate::log_workbench::Changed::Range(since, until) => {
                        view.set_log_options(
                            LogOptions {
                                since_time: *since,
                                until_time: *until,
                                ..view.log_options.clone()
                            },
                            window,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        let mut this = Self {
            session,
            target: ObjectRef::of(&object),
            object,
            rules,
            tab,
            embedded: None,
            log_advanced: false,
            command,
            ran: Exec::Idle,
            shell: None,
            logs: LogBuffer::new(),
            log_options: LogOptions {
                container,
                ..Default::default()
            },
            log_status: LogStatus::Unopened,
            log_scroll: UniformListScrollHandle::new(),
            log_widths: LogLineWidths::default(),
            log_search,
            log_display,
            log_workbench,
            log_wrap_scroll: ListState::new(0, ListAlignment::Top, px(300.)),
            log_wrap: false,
            log_follow: true,
            container_groups,
            container_picker,
            download: Download::Idle,
            download_directory: None,
            download_abort: None,
            _logs_task: None,
            _download_task: None,
            _exec_task: None,
            _container_subscription: subscription,
            _search_subscription: search_subscription,
            _workbench_subscription: workbench_subscription,
        };
        this.select(tab, window, cx);
        this
    }

    pub(crate) fn embed(&mut self, show_name: bool, cx: &mut Context<Self>) {
        if self.embedded.is_none() {
            self.log_workbench
                .update(cx, |view, cx| view.use_compact_controls(cx));
        }
        if self.embedded != Some(show_name) {
            self.embedded = Some(show_name);
            cx.notify();
        }
    }
    pub(crate) fn focus_current(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab {
            PodToolTab::Logs => self.log_search.read(cx).focus_handle(cx).focus(window, cx),
            PodToolTab::Exec => self.command.read(cx).focus_handle(cx).focus(window, cx),
            PodToolTab::Shell => {
                if let Some(shell) = &self.shell {
                    shell.read(cx).focus_handle(cx).focus(window, cx);
                }
            }
        }
    }
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        self._logs_task = None;
        self._exec_task = None;
        if let Some(abort) = self.download_abort.take() {
            abort.abort();
        }
        self._download_task = None;
        self.shell = None;
        cx.notify();
    }

    pub(crate) fn refresh(
        &mut self,
        object: Arc<DynamicObject>,
        rules: Option<Arc<Rules>>,
        cx: &mut Context<Self>,
    ) {
        self.object = object;
        self.rules = rules;
        cx.notify();
    }

    pub(crate) fn select(&mut self, tab: PodToolTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        match tab {
            PodToolTab::Logs if self.log_status == LogStatus::Unopened => {
                self.follow_logs(window, cx)
            }
            PodToolTab::Shell if self.shell.is_none() => self.open_shell(window, cx),
            _ => {}
        }
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match (&self.target.namespace, self.embedded) {
            (Some(namespace), Some(false)) => namespace.clone(),
            (Some(namespace), _) => format!("{namespace} / {}", self.target.name),
            (None, _) => self.target.name.clone(),
        };
        h_flex()
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_1()
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                TabBar::new("pod-tool-tabs")
                    .small()
                    .selected_index(
                        PodToolTab::ALL
                            .iter()
                            .position(|tab| *tab == self.tab)
                            .unwrap_or(0),
                    )
                    .children(
                        PodToolTab::ALL
                            .iter()
                            .map(|tab| Tab::new().child(crate::i18n::text(tab.label()))),
                    )
                    .on_click(cx.listener(|view, index: &usize, window, cx| {
                        if let Some(tab) = PodToolTab::ALL.get(*index).copied() {
                            view.select(tab, window, cx);
                        }
                    })),
            )
            .when(self.embedded.is_none(), |this| {
                this.child(
                    Button::new("close-pod-tools")
                        .xsmall()
                        .ghost()
                        .label("×")
                        .tooltip(crate::i18n::text("Close the Pod tools panel"))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(PodToolsClosed))),
                )
            })
    }

    /// Keep newly added ephemeral containers available without resetting a selection.
    fn sync_container_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let groups = container_groups(&self.object);
        if groups == self.container_groups {
            return;
        }
        let selected = self
            .log_options
            .container
            .clone()
            .filter(|name| groups.iter().any(|group| group.names.contains(name)))
            .or_else(|| default_container(&self.object, &groups));
        self.container_picker.update(cx, |picker, cx| {
            picker.set_items(container_choices(&groups), window, cx);
            if let Some(selected) = &selected {
                picker.set_selected_value(selected, window, cx);
            } else {
                picker.set_selected_index(None, window, cx);
            }
            cx.notify();
        });
        self.container_groups = groups;
        if self.log_options.container != selected {
            self.log_options.container = selected;
            if self.log_status != LogStatus::Unopened {
                self.follow_logs(window, cx);
            }
        }
    }

    fn download_active(&self) -> bool {
        matches!(
            self.download,
            Download::Choosing | Download::Running(_) | Download::Cancelling
        )
    }

    fn download_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.download_active() || self.log_status == LogStatus::Waiting {
            return;
        }
        let Some(namespace) = self.target.namespace.clone() else {
            return;
        };
        let options = self.log_options.clone();
        let name = log_filename(&self.target, &options, beacon_columns::Timestamp::now());
        let directory = self.download_directory.clone().unwrap_or_else(|| {
            directories::UserDirs::new()
                .map(|dirs| dirs.download_dir().unwrap_or(dirs.home_dir()).to_path_buf())
                .unwrap_or_else(std::env::temp_dir)
        });
        let choosing = cx.prompt_for_new_path(&directory, Some(&name));
        let session = self.session.clone();
        let pod = self.target.name.clone();
        let bridge = Bridge::global(cx).clone();
        self.download = Download::Choosing;
        self._download_task = Some(cx.spawn_in(window, async move |this, cx| {
            let choice = choosing
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
            let path = match choice {
                Ok(Some(path)) => path,
                Ok(None) => {
                    let _ = this.update(cx, |view, cx| {
                        view.download = Download::Cancelled;
                        cx.notify();
                    });
                    return;
                }
                Err(error) => {
                    let _ = this.update(cx, |view, cx| {
                        view.download = Download::Failed(
                            crate::i18n::message(
                                "messages.could_not_open_the_save_dialog_error",
                                &[("error", (error).to_string())],
                            )
                            .to_string(),
                        );
                        cx.notify();
                    });
                    return;
                }
            };
            let destination = path.clone();
            let container = options
                .container
                .clone()
                .unwrap_or_else(|| "default container".into());
            let downloading = bridge.run_cancellable(async move {
                session
                    .download_logs(namespace, pod, options, destination)
                    .await
            });
            let abort = downloading.abort_handle();
            if this
                .update(cx, |view, cx| {
                    view.download_directory = path.parent().map(std::path::Path::to_path_buf);
                    view.download_abort = Some(abort);
                    view.download = Download::Running(container);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
            let result = downloading.result().await;
            let _ = this.update(cx, |view, cx| {
                view.download_abort = None;
                view.download = match result {
                    Ok(Ok(bytes)) => Download::Saved { path, bytes },
                    Ok(Err(error)) => Download::Failed(
                        crate::i18n::message(
                            "messages.could_not_download_logs_to_error",
                            &[
                                ("value0", (path.display()).to_string()),
                                ("error", (error).to_string()),
                            ],
                        )
                        .to_string(),
                    ),
                    Err(error) if error.is_cancelled() => Download::Cancelled,
                    Err(error) => Download::Failed(
                        crate::i18n::message(
                            "messages.could_not_download_logs_to_error",
                            &[
                                ("value0", (path.display()).to_string()),
                                ("error", (error).to_string()),
                            ],
                        )
                        .to_string(),
                    ),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_download_status(&self, cx: &App) -> Option<AnyElement> {
        let (text, tone) = match &self.download {
            Download::Idle => return None,
            Download::Choosing => (
                "Choose a folder and filename in the save dialog.".into(),
                Tone::Progressing,
            ),
            Download::Running(container) => (
                crate::i18n::message(
                    "messages.downloading_logs_for_container",
                    &[("container", (container).to_string())],
                )
                .to_string(),
                Tone::Progressing,
            ),
            Download::Cancelling => ("Cancelling log download…".into(), Tone::Progressing),
            Download::Saved { path, bytes } => (
                crate::i18n::message(
                    "messages.saved_logs_to_bytes_bytes",
                    &[
                        ("value0", (path.display()).to_string()),
                        ("bytes", (bytes).to_string()),
                    ],
                )
                .to_string(),
                Tone::Healthy,
            ),
            Download::Cancelled => ("Log download cancelled.".into(), Tone::Unknown),
            Download::Failed(error) => (error.clone(), Tone::Critical),
        };
        Some(
            div()
                .w_full()
                .flex_shrink_0()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(cx.theme().tone(tone))
                .child(crate::copyable_text::copyable_text(
                    "log-download-status",
                    text,
                ))
                .into_any_element(),
        )
    }

    /// (Re)starts the log stream for the current container and options.
    ///
    /// Dropping the previous task drops the stream, which closes the
    /// connection -- a followed log holds one open for as long as anybody is
    /// reading it.
    fn follow_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(namespace) = self.target.namespace.clone() else {
            return;
        };

        self.logs.clear();
        self.log_widths = LogLineWidths::default();
        self.log_scroll = UniformListScrollHandle::new();
        self.log_display.borrow_mut().reset_stream();
        self.log_display.borrow_mut().set_source(format!(
            "{}/{}:{}{}",
            namespace,
            self.target.name,
            self.log_options.container.as_deref().unwrap_or("default"),
            if self.log_options.previous {
                " (previous)"
            } else {
                ""
            }
        ));
        self.log_wrap_scroll.reset(0);
        self.log_follow = true;
        self.log_wrap_scroll.set_follow_mode(FollowMode::Tail);
        self.log_status = LogStatus::Following;

        let stream = self.session.follow_logs(
            namespace,
            self.target.name.clone(),
            self.log_options.clone(),
        );

        self._logs_task = Some(drain_into(
            cx,
            stream,
            |view, event, _window, cx| {
                match event {
                    LogEvent::Waiting => view.log_status = LogStatus::Waiting,
                    LogEvent::Opened => view.log_status = LogStatus::Following,
                    LogEvent::Lines(lines) => {
                        // Follow the tail, but only while the reader is at it.
                        // Yanking somebody back to the bottom because a line
                        // arrived while they were reading history is the worst
                        // thing a log pane can do.
                        let follow = view.log_follow && view.is_at_tail();
                        view.logs.extend(lines);
                        view.update_log_display(cx);
                        if follow {
                            view.scroll_logs_to_end();
                        }
                    }
                    LogEvent::Closed => view.log_status = LogStatus::Ended,
                    LogEvent::Failed(error) => view.log_status = LogStatus::Failed(error),
                }
                cx.notify();
            },
            window,
        ));
        cx.notify();
    }

    /// Whether the log view is scrolled to the newest line.
    ///
    /// Before the first layout there is nothing to measure, and the answer is
    /// yes: a pane that has not been scrolled is at the bottom of an empty
    /// list.
    fn is_at_tail(&self) -> bool {
        if self.log_wrap {
            // Most virtualized rows have unknown heights. Tail state records
            // user scrolling without needing to measure the entire stream.
            return self.log_wrap_scroll.is_following_tail();
        }
        let state = self.log_scroll.0.borrow();
        let offset = state.base_handle.offset().y;
        let max = state.base_handle.max_offset().y;

        if max <= px(0.) {
            return true;
        }
        // Offsets are negative as the list scrolls down, and a line height of
        // slack keeps "near enough" from meaning "only exactly".
        let from_bottom = max + offset;
        from_bottom <= px(24.)
    }

    fn update_log_display(&mut self, cx: &mut App) {
        let old_top = self.unwrapped_log_top();
        let filter = self.log_workbench.read(cx).filter(cx);
        let change = self
            .log_display
            .borrow_mut()
            .update_with(&self.logs, &filter);
        self.log_workbench.update(cx, |view, cx| view.refresh(cx));
        if change.reset {
            self.log_scroll = UniformListScrollHandle::new();
            self.log_wrap_scroll
                .reset(self.log_display.borrow().rows.len());
        } else {
            if change.removed > 0 {
                self.log_wrap_scroll.splice(0..change.removed, 0);
                self.log_scroll
                    .scroll_to_item(old_top.saturating_sub(change.removed), ScrollStrategy::Top);
            }
            if change.added > 0 {
                let count = self.log_wrap_scroll.item_count();
                self.log_wrap_scroll.splice(count..count, change.added);
            }
        }
    }

    fn scroll_logs_to_end(&self) {
        let count = self.log_display.borrow().rows.len();
        if count > 0 {
            self.log_scroll
                .scroll_to_item(count - 1, ScrollStrategy::Bottom);
            self.log_wrap_scroll.scroll_to_end();
        }
    }

    fn set_log_follow(&mut self, follow: bool) {
        self.log_follow = follow;
        self.log_wrap_scroll.set_follow_mode(if follow {
            FollowMode::Tail
        } else {
            FollowMode::Normal
        });
        if follow {
            self.scroll_logs_to_end();
        }
    }

    fn unwrapped_log_top(&self) -> usize {
        let scroll = self.log_scroll.0.borrow();
        scroll
            .deferred_scroll_to_item
            .as_ref()
            .map(|item| item.item_index)
            .unwrap_or_else(|| scroll.base_handle.logical_scroll_top().0)
    }

    fn toggle_log_wrap(&mut self, cx: &mut Context<Self>) {
        let at_tail = self.is_at_tail();
        let index = if self.log_wrap {
            self.log_wrap_scroll.logical_scroll_top().item_ix
        } else {
            self.unwrapped_log_top()
        };
        self.log_wrap = !self.log_wrap;
        self.log_scroll.scroll_to_item(index, ScrollStrategy::Top);
        self.log_wrap_scroll.scroll_to(ListOffset {
            item_ix: index,
            offset_in_item: px(0.),
        });
        if self.log_follow && at_tail {
            self.set_log_follow(true);
        }
        cx.notify();
    }

    fn set_log_options(
        &mut self,
        options: LogOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.log_options == options {
            return;
        }
        self.log_options = options;
        self.follow_logs(window, cx);
    }

    /// Whether the shell inside this panel is what has focus.
    ///
    /// Escape belongs to a terminal while the terminal is being typed into --
    /// a shell that swallowed it would be no shell at all -- so this is the
    /// one case where closing the panel is the wrong thing to do.
    pub fn shell_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.tab == PodToolTab::Shell
            && self
                .shell
                .as_ref()
                .is_some_and(|shell| shell.read(cx).focus_handle(cx).is_focused(window))
    }

    fn render_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.target.namespace.is_none() {
            return self.notice("A pod outside a namespace has no logs.", Tone::Unknown, cx);
        }

        let total = self.logs.len();
        let count = self.log_display.borrow().rows.len();
        let dropped = self.logs.dropped();

        let body: AnyElement = match &self.log_status {
            LogStatus::Failed(error) if count == 0 => {
                self.notice(error.clone(), Tone::Critical, cx)
            }
            LogStatus::Unopened => self.notice("Opening the stream…", Tone::Progressing, cx),
            LogStatus::Waiting => {
                self.notice("Waiting for container to start…", Tone::Progressing, cx)
            }
            _ if total == 0 && count == 0 => self.notice("Nothing logged yet.", Tone::Unknown, cx),
            _ if count == 0 => self.notice("No log lines match this search.", Tone::Unknown, cx),
            _ => LogCanvas {
                workbench: Some(self.log_workbench.clone()),
                display: self.log_display.clone(),
                wrap: self.log_wrap,
                wrapped: &self.log_wrap_scroll,
                unwrapped: &self.log_scroll,
                widths: &mut self.log_widths,
            }
            .render(window, cx),
        };

        v_flex()
            .size_full()
            .when(self.embedded.is_none(), |pane| pane.child(
                h_flex()
                    .id("pod-log-toolbar")
                    .w_full()
                    .min_w_0()
                    .overflow_x_scrollbar()
                    .whitespace_nowrap()
                    .flex_shrink_0()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .child(div().w(px(250.)).flex_shrink_0().child(
                        Select::new(&self.container_picker)
                            .id("pod-log-container")
                            .xsmall()
                            .w_full()
                            .menu_width(px(300.))
                            .title_prefix(crate::i18n::text("Container: "))
                            .placeholder(crate::i18n::text("Select container"))
                            .accessibility_label(crate::i18n::text("Log container"))
                            .search_placeholder(crate::i18n::text("Search containers"))
                            .disabled(self.container_groups.is_empty())
                    ))
                    .child(div().w(px(180.)).flex_shrink_0().child(Input::new(&self.log_search).id("pod-log-search").xsmall()))
                    .child(div().flex_1())
                    .child(Button::new("pause-log-follow").xsmall()
                        .when(!self.log_follow, |button| button.primary())
                        .when(self.log_follow, |button| button.ghost())
                        .label(crate::i18n::text(if self.log_follow { "Pause follow" } else { "Resume follow" }))
                        .tooltip(crate::i18n::text("Pause automatic scrolling; logs continue to arrive"))
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.set_log_follow(!view.log_follow);
                            cx.notify();
                        })))
                    .child(Button::new("latest-log-line").xsmall().ghost().label(crate::i18n::text("Latest"))
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.set_log_follow(true);
                            cx.notify();
                        })))
                    .child(Button::new("wrap-log-lines").xsmall()
                        .when(self.log_wrap, |button| button.primary())
                        .when(!self.log_wrap, |button| button.ghost()).label(crate::i18n::text("Wrap"))
                        .on_click(cx.listener(|view, _, _, cx| view.toggle_log_wrap(cx))))
                    .child(
                        Button::new("previous")
                            .xsmall()
                            .when(self.log_options.previous, |button| button.primary())
                            .when(!self.log_options.previous, |button| button.ghost())
                            .label(crate::i18n::text("Previous"))
                            .on_click(cx.listener(|view, _, window, cx| {
                                let options = LogOptions {
                                    previous: !view.log_options.previous,
                                    ..view.log_options.clone()
                                };
                                view.set_log_options(options, window, cx);
                            })),
                    )
                    .child(
                        Button::new("timestamps")
                            .xsmall()
                            .when(self.log_options.timestamps, |button| button.primary())
                            .when(!self.log_options.timestamps, |button| button.ghost())
                            .label(crate::i18n::text("Timestamps"))
                            .on_click(cx.listener(|view, _, window, cx| {
                                let options = LogOptions {
                                    timestamps: !view.log_options.timestamps,
                                    ..view.log_options.clone()
                                };
                                view.set_log_options(options, window, cx);
                            })),
                    )
                    .child(
                        Button::new("download-logs")
                            .xsmall()
                            .ghost()
                            .label(crate::i18n::text("Export all…"))
                            .disabled(self.download_active() || self.log_options.container.is_none() || self.log_status == LogStatus::Waiting)
                            .tooltip(crate::i18n::text("Export this container's complete available logs for the time range; ignores local filters. Choose a folder and filename"))
                            .on_click(cx.listener(|view, _, window, cx| view.download_logs(window, cx))),
                    )
                    .when(matches!(self.download, Download::Running(_)), |this| {
                        this.child(Button::new("cancel-log-download").xsmall().ghost().label(crate::i18n::text("Cancel"))
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some(abort) = &view.download_abort {
                                    abort.abort();
                                    view.download = Download::Cancelling;
                                    cx.notify();
                                }
                            })))
                    })
                    .child(
                        Button::new("copy-logs")
                            .xsmall()
                            .ghost()
                            .label(crate::i18n::text("Copy"))
                            .tooltip(crate::i18n::text("Copy matching retained raw log lines; selected display fields do not remove original data"))
                            .on_click(cx.listener(|view, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    view.log_display.borrow().text(),
                                ));
                            })),
                    )
                    .child(div().flex_shrink_0().text_color(cx.theme().muted_foreground).child(
                        match (dropped, &self.log_status) {
                            // Saying so matters: what is shown starts
                            // mid-stream, and nothing else would say that.
                            (0, LogStatus::Ended) => crate::i18n::message("messages.count_total_lines_ended", &[("count", (count).to_string()), ("total", (total).to_string())]).to_string(),
                            (0, _) => crate::i18n::message("messages.count_total_lines", &[("count", (count).to_string()), ("total", (total).to_string())]).to_string(),
                            (dropped, _) => {
                                crate::i18n::message("messages.count_total_lines_dropped_older_dropped", &[("count", (count).to_string()), ("total", (total).to_string()), ("dropped", (dropped).to_string())]).to_string()
                            }
                        },
                    )),
            ))
            .when(self.embedded.is_some(), |pane| pane.child(self.workspace_log_toolbar(cx)))
            .when(self.embedded.is_none() || self.log_advanced, |pane| pane.child(self.log_workbench.clone()))
            .when(matches!(self.log_status, LogStatus::Failed(_)) && count > 0, |pane| pane.child(div().px_3().py_1().text_xs().text_color(cx.theme().danger).child(crate::copyable_text::copyable_text("pod-log-stream-error", match &self.log_status { LogStatus::Failed(error) => error.clone(), _ => String::new() }))))
            .children(self.render_download_status(cx))
            .child(div().flex_1().min_size_0().overflow_hidden().child(body))
            .when(self.embedded.is_some(), |pane| pane.child(self.workspace_log_footer(cx)))
            .children(self.log_workbench.read(cx).inspector())
            .into_any_element()
    }

    /// Runs the typed command in the pod.
    fn run_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(namespace) = self.target.namespace.clone() else {
            return;
        };
        let typed = self.command.read(cx).value().to_string();
        let command = beacon_kube::exec::split(&typed);
        if command.is_empty() {
            return;
        }

        self.ran = Exec::Running;
        cx.notify();

        let session = self.session.clone();
        let pod = self.target.name.clone();
        let container = self.log_options.container.clone();
        let running = Bridge::global(cx)
            .run(async move { session.exec(namespace, pod, container, command).await });

        self._exec_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = running.await;
            let _ = this.update(cx, |view, cx| {
                view.ran = match result {
                    Ok(Ok(output)) => Exec::Done(Box::new(output)),
                    Ok(Err(error)) => Exec::Failed(beacon_kube::error::diagnose(&error)),
                    Err(error) => Exec::Failed(error.to_string()),
                };
                cx.notify();
            });
        }));
    }

    /// Builds the terminal pane. The shell itself is not started until the
    /// user asks -- opening a tab should not run something in a container.
    fn open_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(namespace) = self.target.namespace.clone() else {
            return;
        };
        let session = self.session.clone();
        let pod = self.target.name.clone();
        let container = self.log_options.container.clone();

        self.shell = Some(cx.new(|cx| {
            crate::terminal::TerminalView::new(session, namespace, pod, container, window, cx)
        }));
    }

    fn render_shell(&self, cx: &mut Context<Self>) -> AnyElement {
        // Said here rather than let the connection fail: a refusal arrives as
        // "failed to switch protocol: 403 Forbidden", which names neither the
        // permission nor the namespace it is missing in.
        if !crate::actions::may_exec(self.rules.as_deref()) {
            return self.notice(
                "You may not exec into pods in this namespace: check `get` and `create` on `pods/exec`.",
                Tone::Warning,
                cx,
            );
        }

        match self.shell.clone() {
            Some(shell) => div().size_full().child(shell).into_any_element(),
            None => self.notice("A pod outside a namespace has no shell.", Tone::Unknown, cx),
        }
    }

    fn render_exec(&self, cx: &mut Context<Self>) -> AnyElement {
        if !crate::actions::may_exec(self.rules.as_deref()) {
            return self.notice(
                "You may not run commands in pods here: check `get` and `create` on `pods/exec`.",
                Tone::Warning,
                cx,
            );
        }

        let running = matches!(self.ran, Exec::Running);

        let output: AnyElement = match &self.ran {
            Exec::Idle => self.notice(
                "Runs one command and shows its output. Use Shell for an interactive session.",
                Tone::Unknown,
                cx,
            ),
            Exec::Running => self.notice("Running…", Tone::Progressing, cx),
            Exec::Failed(error) => self.notice(error.clone(), Tone::Critical, cx),
            Exec::Done(output) => {
                let text = output.combined();
                div()
                    .id("exec-output")
                    .size_full()
                    .overflow_scrollbar()
                    .p_3()
                    .child(
                        v_flex()
                            .gap_2()
                            .children(output.note.clone().map(|note| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().tone(Tone::Warning))
                                    .child(crate::copyable_text::copyable_text("exec-note", note))
                            }))
                            .child(div().font_family("monospace").text_xs().child(
                                crate::copyable_text::copyable_text(
                                    "exec-text",
                                    if text.is_empty() {
                                        "(no output)".to_string()
                                    } else {
                                        text
                                    },
                                ),
                            )),
                    )
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.command).small()),
                    )
                    .child(
                        Button::new("run")
                            .primary()
                            .xsmall()
                            .label(crate::i18n::text(if running {
                                "Running…"
                            } else {
                                "Run"
                            }))
                            .disabled(running)
                            .on_click(
                                cx.listener(|view, _, window, cx| view.run_command(window, cx)),
                            ),
                    ),
            )
            .child(div().flex_1().overflow_hidden().child(output))
            .into_any_element()
    }

    fn notice(
        &self,
        message: impl Into<SharedString>,
        tone: Tone,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let waiting = tone == Tone::Progressing;
        let message = message.into();
        let permissions = crate::permissions::error_link(
            "pod-tools-permissions",
            self.session.clone(),
            &message,
            beacon_kube::rbac::Query {
                verb: if self.tab == PodToolTab::Logs {
                    "get".into()
                } else {
                    "create".into()
                },
                resource: "pods".into(),
                subresource: if self.tab == PodToolTab::Logs {
                    "log".into()
                } else {
                    "exec".into()
                },
                namespace: self.target.namespace.clone(),
                name: Some(self.target.name.clone()),
                ..Default::default()
            },
        );

        h_flex()
            .id("pod-tools-status")
            .test_support()
            .role(Role::Status)
            .aria_label(crate::i18n::text(&message))
            .size_full()
            .p_6()
            .gap_2()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().tone(tone))
            // A Progressing notice is by definition a wait, and a line of
            // static text is the one thing that cannot say whether anything is
            // still happening.
            .when(waiting, |this| {
                this.child(Spinner::new().small().color(cx.theme().tone(tone)))
            })
            .child(crate::copyable_text::copyable_text(
                "pod-tools-notice",
                crate::i18n::text(&message),
            ))
            .children(permissions)
            .into_any_element()
    }
}

impl Render for PodToolsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_container_picker(window, cx);
        let body = match self.tab {
            PodToolTab::Logs => self.render_logs(window, cx),
            PodToolTab::Exec => self.render_exec(cx),
            PodToolTab::Shell => self.render_shell(cx),
        };
        v_flex()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod rendering_tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;

    #[::core::prelude::v1::test]
    fn pending_container_logs_wait_in_both_languages_and_open_automatically() {
        use crate::{
            feature_test_support as support,
            i18n::{self, Language},
        };
        let directory = tempfile::tempdir().unwrap();
        let cx = &mut support::context();
        support::workspace(cx, directory.path());
        let mut pod = serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"starting","namespace":"default","uid":"starting-logs"},"spec":{"containers":[{"name":"app"}]},"status":{"phase":"Pending","containerStatuses":[{"name":"app","restartCount":0,"state":{"waiting":{"reason":"ContainerCreating"}}}]}});
        let (fixture, session) = support::fixture(cx, "waiting-logs", vec![pod.clone()]);
        let object = Arc::new(serde_json::from_value(pod.clone()).unwrap());
        let (window, view) = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1400.), px(900.)),
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| PodToolsView::new(session, object, None, PodToolTab::Logs, window, cx))
            })
            .unwrap()
        });
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| view.log_status == LogStatus::Waiting)
        });
        for (language, label) in [
            (Language::English, "Waiting for container to start…"),
            (Language::SimplifiedChinese, "正在等待容器启动…"),
        ] {
            cx.update(|cx| i18n::apply(language, cx));
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("pod-tools-status").label(), Some(label));
                window.click("download-logs", cx);
                assert!(matches!(view.read(cx).download, Download::Idle));
            })
            .unwrap();
        }
        pod["status"]["phase"] = serde_json::json!("Running");
        pod["status"]["containerStatuses"][0]["state"] = serde_json::json!({"running":{}});
        fixture.put(pod);
        // The network runtime owns the retry timer; pump foreground work until
        // it succeeds, without requiring a manual refresh or another tab.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while std::time::Instant::now() < deadline
            && view.read_with(cx, |view, _| view.log_status != LogStatus::Ended)
        {
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(view.read_with(cx, |view, _| view.log_status == LogStatus::Ended));
        assert!(view.read_with(cx, |view, _| view.logs.to_text().contains("fixture log")));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("download-logs", cx);
            assert!(matches!(view.read(cx).download, Download::Choosing));
        })
        .unwrap();
        cx.update(|cx| i18n::apply(Language::English, cx));
    }

    #[::core::prelude::v1::test]
    fn language_switch_updates_log_controls_without_changing_container_or_log_options() {
        use crate::{
            feature_test_support as support,
            i18n::{self, Language},
        };
        let directory = tempfile::tempdir().unwrap();
        let cx = &mut support::context();
        support::workspace(cx, directory.path());
        let object = serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"Save","namespace":"default","uid":"language-pod"},"spec":{"containers":[{"name":"app"}]}});
        let (_fixture, session) = support::fixture(cx, "language-logs", vec![object.clone()]);
        let object = Arc::new(serde_json::from_value(object).unwrap());
        let (handle, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| PodToolsView::new(session, object, None, PodToolTab::Logs, window, cx))
            })
            .unwrap()
        });
        for (language, pause, previous) in [
            (Language::English, "Pause follow", "Previous"),
            (Language::SimplifiedChinese, "暂停跟随", "上一个容器"),
            (Language::English, "Pause follow", "Previous"),
        ] {
            cx.update(|cx| i18n::apply(language, cx));
            for _ in 0..3 {
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
            }
            cx.update_window(handle, |_, window, cx| {
                assert_eq!(window.find("pause-log-follow").label(), Some(pause));
                assert_eq!(window.find("previous").label(), Some(previous));
                let view = view.read(cx);
                assert_eq!(view.target.name, "Save");
                assert_eq!(view.log_options.container.as_deref(), Some("app"));
                assert!(!view.log_options.previous);
            })
            .unwrap();
        }
    }

    struct Pane {
        logs: LogBuffer,
        display: Rc<RefCell<crate::pod_logs::Display>>,
        wrap: bool,
        width: Pixels,
        wrapped: ListState,
        unwrapped: UniformListScrollHandle,
        widths: LogLineWidths,
    }
    impl Render for Pane {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().id("test-log-pane").w(self.width).h(px(350.)).child(
                LogCanvas {
                    workbench: None,
                    display: self.display.clone(),
                    wrap: self.wrap,
                    wrapped: &self.wrapped,
                    unwrapped: &self.unwrapped,
                    widths: &mut self.widths,
                }
                .render(window, cx),
            )
        }
    }

    #[::core::prelude::v1::test]
    fn structured_controls_apply_to_pod_logs_without_losing_previous_or_container() {
        use crate::feature_test_support as support;
        let cx = &mut support::context();
        let object = serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"demo","namespace":"default","uid":"pod-log-demo"},"spec":{"containers":[{"name":"app"},{"name":"sidecar"}]}});
        let (fixture, session) = support::fixture(cx, "structured-pod", vec![object.clone()]);
        let object = Arc::new(serde_json::from_value(object).unwrap());
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(1400.), px(900.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| {
                        PodToolsView::new(session, object, None, PodToolTab::Logs, window, cx)
                    })
                },
            )
            .unwrap()
        });
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| view.log_status == LogStatus::Ended)
        });
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.logs.extend(
                    [
                        "{\"level\":\"error\",\"msg\":\"failed\"}",
                        "level=info msg=ready",
                    ]
                    .map(str::to_string),
                );
                view.update_log_display(cx);
            });
            window.render_frame(cx);
            window.click("log-level-Error", cx);
            window.render_frame(cx);
            assert!(window.find(("inspect-log", 1usize)).visible());
            window.click("previous", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| view.log_status == LogStatus::Ended)
        });
        assert!(view.read_with(cx, |view, _| view.log_options.previous));
        assert_eq!(
            view.read_with(cx, |view, _| view.log_options.container.clone())
                .as_deref(),
            Some("app")
        );
        cx.update_window(window, |_, _, cx| {
            let workbench = view.read(cx).log_workbench.clone();
            workbench.update(cx, |_, cx| {
                cx.emit(crate::log_workbench::Changed::Range(
                    Some("2026-10-09T00:00:00Z".parse().unwrap()),
                    Some("2026-10-11T00:00:00Z".parse().unwrap()),
                ))
            });
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| view.log_status == LogStatus::Ended)
        });
        let requests = fixture.requests.lock().unwrap();
        let uri = requests
            .iter()
            .find(|(uri, _)| uri.contains("sinceTime"))
            .unwrap()
            .0
            .split_whitespace()
            .nth(1)
            .unwrap();
        let url = url::Url::parse(&format!("http://fixture{uri}")).unwrap();
        let query = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            query.get("container").map(|value| value.as_ref()),
            Some("app")
        );
        assert_eq!(
            query.get("previous").map(|value| value.as_ref()),
            Some("true")
        );
        assert_eq!(
            query.get("timestamps").map(|value| value.as_ref()),
            Some("true")
        );
        assert!(!query.contains_key("tailLines"));
    }

    #[::core::prelude::v1::test]
    fn log_canvas_wraps_long_lines_reflows_and_keeps_horizontal_scroll_without_wrap() {
        let cx = &mut TestAppContext::single();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let (window, pane) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                let mut logs = LogBuffer::new();
                logs.extend([
                    "ERROR ".repeat(30),
                    "ERROR short".to_owned(),
                    "f".repeat(180),
                ]);
                let mut display = crate::pod_logs::Display::default();
                display.update(&logs, "");
                cx.new(|_| Pane {
                    logs,
                    display: Rc::new(RefCell::new(display)),
                    wrap: false,
                    width: px(300.),
                    wrapped: ListState::new(3, ListAlignment::Top, px(300.)),
                    unwrapped: UniformListScrollHandle::new(),
                    widths: LogLineWidths::default(),
                })
            })
            .unwrap()
        });
        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .unwrap()
        };
        draw(cx);
        assert!(
            pane.read_with(cx, |pane, _| pane
                .unwrapped
                .0
                .borrow()
                .base_handle
                .max_offset()
                .x)
                > px(0.)
        );
        pane.update(cx, |pane, cx| {
            pane.wrap = true;
            cx.notify();
        });
        draw(cx);
        let wide_height = cx
            .update_window(window, |_, window, _| {
                let first = window.find(("pod-log-line", 0usize)).bounds();
                let second = window.find(("pod-log-line", 1usize)).bounds();
                assert!(first.size.height > px(18.));
                assert!(second.origin.y >= first.bottom());
                first.size.height
            })
            .unwrap();
        pane.update(cx, |pane, cx| {
            pane.width = px(180.);
            cx.notify();
        });
        draw(cx);
        cx.update_window(window, |_, window, _| {
            assert!(window.find(("pod-log-line", 0usize)).bounds().size.height > wide_height);
        })
        .unwrap();
        pane.update(cx, |pane, cx| {
            pane.display.borrow_mut().update(&pane.logs, "error");
            pane.wrapped.reset(2);
            cx.notify();
        });
        draw(cx);
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.display.borrow().text()),
            format!("{}\nERROR short", "ERROR ".repeat(30))
        );
        pane.update(cx, |pane, cx| {
            pane.logs.push("ERROR new".into());
            let change = pane.display.borrow_mut().update(&pane.logs, "error");
            assert_eq!(change.added, 1);
            let offset = pane.wrapped.logical_scroll_top();
            pane.wrapped.splice(2..2, 1);
            assert_eq!(
                (
                    pane.wrapped.logical_scroll_top().item_ix,
                    pane.wrapped.logical_scroll_top().offset_in_item
                ),
                (offset.item_ix, offset.offset_in_item),
                "arriving logs preserve a paused scroll position"
            );
            pane.wrapped.scroll_to_end();
            cx.notify();
        });
        draw(cx);
        cx.update_window(window, |_, window, _| {
            assert!(
                window.find(("pod-log-line", 3usize)).visible(),
                "Latest reveals the newest matching line even when all lines fit"
            );
        })
        .unwrap();
    }

    #[::core::prelude::v1::test]
    fn wrapped_logs_follow_and_pause_without_measuring_all_rows() {
        let cx = &mut TestAppContext::single();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let (window, pane) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                let mut logs = LogBuffer::new();
                logs.extend((0..5000).map(|index| format!("line {index}")));
                let mut display = crate::pod_logs::Display::default();
                display.update(&logs, "");
                let wrapped = ListState::new(5000, ListAlignment::Top, px(300.));
                wrapped.set_follow_mode(FollowMode::Tail);
                cx.new(|_| Pane {
                    logs,
                    display: Rc::new(RefCell::new(display)),
                    wrap: true,
                    width: px(300.),
                    wrapped,
                    unwrapped: UniformListScrollHandle::new(),
                    widths: LogLineWidths::default(),
                })
            })
            .unwrap()
        });
        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .unwrap()
        };
        draw(cx);
        pane.update(cx, |pane, cx| {
            pane.wrapped.scroll_by(px(-200.));
            cx.notify();
        });
        draw(cx);
        let before = pane.read_with(cx, |pane, _| {
            assert!(!pane.wrapped.is_following_tail());
            assert!(
                pane.wrapped.is_scrolled_to_end().is_none(),
                "off-screen heights remain unmeasured"
            );
            pane.wrapped.logical_scroll_top().item_ix
        });
        pane.update(cx, |pane, cx| {
            pane.logs.push("line 5000".into());
            pane.display.borrow_mut().update(&pane.logs, "");
            pane.wrapped.splice(5000..5000, 1);
            cx.notify();
        });
        draw(cx);
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.wrapped.logical_scroll_top().item_ix),
            before
        );
        pane.update(cx, |pane, cx| {
            pane.wrapped.set_follow_mode(FollowMode::Normal);
            pane.wrapped.scroll_to_end();
            cx.notify();
        });
        draw(cx);
        let before = pane.read_with(cx, |pane, _| pane.wrapped.logical_scroll_top().item_ix);
        pane.update(cx, |pane, cx| {
            pane.logs.push("line 5001".into());
            pane.display.borrow_mut().update(&pane.logs, "");
            pane.wrapped.splice(5001..5001, 1);
            cx.notify();
        });
        draw(cx);
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.wrapped.logical_scroll_top().item_ix),
            before
        );
        pane.update(cx, |pane, cx| {
            pane.wrapped.set_follow_mode(FollowMode::Tail);
            cx.notify();
        });
        draw(cx);
        cx.update_window(window, |_, window, _| {
            assert!(window.find(("pod-log-line", 5001usize)).visible())
        })
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::{LogLineWidths, container_groups, default_container, log_filename};
    use beacon_kube::{DynamicObject, LogBuffer, LogOptions, ObjectRef};
    use gpui_kit::{font, px};
    use serde_json::json;

    #[test]
    fn log_containers_include_init_and_ephemeral_and_honor_the_default_annotation() {
        let mut pod: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "demo", "namespace": "default", "annotations": {
                "kubectl.kubernetes.io/default-container": "app"
            }},
            "spec": {
                "containers": [{"name": "sidecar"}, {"name": "app"}],
                "initContainers": [{"name": "setup"}],
                "ephemeralContainers": [{"name": "debug"}]
            }
        }))
        .unwrap();
        let groups = container_groups(&pod);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].names, ["sidecar", "app"]);
        assert_eq!(groups[1].title, "Init containers");
        assert_eq!(groups[1].names, ["setup"]);
        assert_eq!(groups[2].title, "Ephemeral containers");
        assert_eq!(groups[2].names, ["debug"]);
        assert_eq!(default_container(&pod, &groups).as_deref(), Some("app"));

        pod.metadata.annotations.as_mut().unwrap().insert(
            "kubectl.kubernetes.io/default-container".into(),
            "missing".into(),
        );
        assert_eq!(default_container(&pod, &groups).as_deref(), Some("sidecar"));
        pod.data["spec"] = json!({"containers": [{"name": "only"}]});
        let groups = container_groups(&pod);
        assert_eq!(default_container(&pod, &groups).as_deref(), Some("only"));
    }

    #[test]
    fn log_download_names_are_portable_and_identify_the_source() {
        let now = "2026-10-09T07:08:09Z".parse().unwrap();
        let options = LogOptions {
            container: Some("app".into()),
            previous: true,
            timestamps: true,
            ..Default::default()
        };
        assert_eq!(
            log_filename(
                &ObjectRef::new(Some("default".into()), "demo"),
                &options,
                now
            ),
            "pod_default_demo_app_20261009-070809-UTC_previous.log"
        );
        let name = log_filename(
            &ObjectRef::new(Some("n".repeat(63)), "p".repeat(253)),
            &LogOptions {
                container: Some("unsafe:/\\ name".repeat(20)),
                ..options
            },
            now,
        );
        assert!(name.len() < 255);
        assert!(
            name.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
        );
    }

    #[test]
    fn log_widths_use_measured_geometry_and_only_measure_new_lines() {
        let mut logs = LogBuffer::new();
        logs.extend(["short", "a longer ASCII line", "中文"].map(str::to_string));
        let mut widths = LogLineWidths::default();
        let font = font("monospace");
        let mut measured = 0;
        let mut measure = |line: &str| {
            measured += 1;
            px(if line == "中文" {
                200.
            } else {
                line.len() as f32
            })
        };
        assert_eq!(
            widths.widest(&logs, font.clone(), px(12.), &mut measure),
            Some(2)
        );
        assert_eq!(
            widths.widest(&logs, font.clone(), px(12.), &mut measure),
            Some(2)
        );
        logs.push("new line".to_string());
        assert_eq!(
            widths.widest(&logs, font.clone(), px(12.), &mut measure),
            Some(2)
        );
        assert_eq!(measured, 4);

        let mut measured = 0;
        assert_eq!(
            widths.widest(&logs, font, px(14.), |line| {
                measured += 1;
                px(line.len() as f32)
            }),
            Some(1)
        );
        assert_eq!(measured, 4);
    }

    #[test]
    fn dropping_the_widest_line_updates_the_scroll_range_without_reshaping_history() {
        let mut logs = LogBuffer::new();
        logs.push("the widest old line".to_string());
        logs.extend(std::iter::repeat_n("x".to_string(), 49_999));
        let mut widths = LogLineWidths::default();
        let font = font("monospace");
        assert_eq!(
            widths.widest(&logs, font.clone(), px(12.), |line| px(line.len() as f32)),
            Some(0)
        );

        logs.push("tail".to_string());
        assert_eq!(logs.dropped(), 1);
        let mut measured = 0;
        assert_eq!(
            widths.widest(&logs, font, px(12.), |line| {
                measured += 1;
                px(line.len() as f32)
            }),
            Some(49_999)
        );
        assert_eq!(measured, 1);
    }
}
