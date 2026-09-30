//! Pod logs, commands and interactive shells in the independent bottom panel.

use std::sync::Arc;

use beacon_kube::{
    ClusterSession, DynamicObject, LogBuffer, LogEvent, LogOptions, ObjectRef, Rules,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::bridge::{Bridge, drain_into};
use crate::theme::{BeaconTheme as _, Tone};

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
    command: Entity<InputState>,
    ran: Exec,
    shell: Option<Entity<crate::terminal::TerminalView>>,
    logs: LogBuffer,
    log_options: LogOptions,
    log_status: LogStatus,
    log_scroll: UniformListScrollHandle,
    _logs_task: Option<Task<()>>,
    _exec_task: Option<Task<()>>,
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
    Following,
    /// The stream ended, which for a followed log means the container did.
    Ended,
    Failed(String),
}

impl PodToolsView {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        object: Arc<DynamicObject>,
        rules: Option<Arc<Rules>>,
        tab: PodToolTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let command = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("A command to run, e.g. ls -la /etc")
                .default_value("ls -la /")
        });
        let mut this = Self {
            session,
            target: ObjectRef::of(&object),
            object,
            rules,
            tab,
            command,
            ran: Exec::Idle,
            shell: None,
            logs: LogBuffer::new(),
            log_options: LogOptions::default(),
            log_status: LogStatus::Unopened,
            log_scroll: UniformListScrollHandle::new(),
            _logs_task: None,
            _exec_task: None,
        };
        this.select(tab, window, cx);
        this
    }

    pub(crate) fn target(&self) -> &ObjectRef {
        &self.target
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
        let title = match &self.target.namespace {
            Some(namespace) => format!("{namespace} / {}", self.target.name),
            None => self.target.name.clone(),
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
                            .map(|tab| Tab::new().child(tab.label())),
                    )
                    .on_click(cx.listener(|view, index: &usize, window, cx| {
                        if let Some(tab) = PodToolTab::ALL.get(*index).copied() {
                            view.select(tab, window, cx);
                        }
                    })),
            )
            .child(
                Button::new("close-pod-tools")
                    .xsmall()
                    .ghost()
                    .label("×")
                    .tooltip("Close the Pod tools panel")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PodToolsClosed))),
            )
    }

    /// The containers this pod declares, for the log picker.
    fn container_names(&self) -> Vec<String> {
        self.object
            .data
            .get("spec")
            .and_then(|spec| spec.get("containers"))
            .and_then(Value::as_array)
            .map(|containers| {
                containers
                    .iter()
                    .filter_map(|container| container.get("name")?.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
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
                    LogEvent::Lines(lines) => {
                        // Follow the tail, but only while the reader is at it.
                        // Yanking somebody back to the bottom because a line
                        // arrived while they were reading history is the worst
                        // thing a log pane can do.
                        let follow = view.is_at_tail();
                        view.logs.extend(lines);
                        if follow && !view.logs.is_empty() {
                            view.log_scroll
                                .scroll_to_item(view.logs.len() - 1, ScrollStrategy::Bottom);
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

    fn render_logs(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.target.namespace.is_none() {
            return self.notice("A pod outside a namespace has no logs.", Tone::Unknown, cx);
        }

        let containers = self.container_names();
        let selected = self.log_options.container.clone();
        let count = self.logs.len();
        let dropped = self.logs.dropped();

        let body: AnyElement = match &self.log_status {
            LogStatus::Failed(error) => self.notice(error.clone(), Tone::Critical, cx),
            LogStatus::Unopened => self.notice("Opening the stream…", Tone::Progressing, cx),
            _ if count == 0 => self.notice("Nothing logged yet.", Tone::Unknown, cx),
            _ => {
                // One line per row, never wrapped: a wrapped line has no fixed
                // height, and a fixed height is what lets fifty thousand of
                // them scroll at all.
                let lines: Vec<SharedString> = self
                    .logs
                    .lines()
                    .map(|line| SharedString::from(line.to_string()))
                    .collect();

                uniform_list("log-lines", lines.len(), move |range, _, _| {
                    range
                        .filter_map(|index| lines.get(index).cloned())
                        .map(|line| {
                            div()
                                .px_3()
                                .font_family("monospace")
                                .text_xs()
                                .whitespace_nowrap()
                                .child(line)
                        })
                        .collect()
                })
                .track_scroll(&self.log_scroll)
                .size_full()
                .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .flex_shrink_0()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    // A pod with one container needs no picker; one with three
                    // is unreadable without it.
                    .when(containers.len() > 1, |this| {
                        this.children(containers.into_iter().enumerate().map(
                            |(index, container)| {
                                // With no container chosen the API serves the
                                // first one, so that is the one shown as active.
                                let active = match selected.as_deref() {
                                    Some(chosen) => chosen == container,
                                    None => index == 0,
                                };
                                let name = container.clone();
                                Button::new(SharedString::from(format!("container-{container}")))
                                    .xsmall()
                                    .when(active, |button| button.primary())
                                    .when(!active, |button| button.ghost())
                                    .label(container)
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        let options = LogOptions {
                                            container: Some(name.clone()),
                                            ..view.log_options.clone()
                                        };
                                        view.set_log_options(options, window, cx);
                                    }))
                            },
                        ))
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("previous")
                            .xsmall()
                            .when(self.log_options.previous, |button| button.primary())
                            .when(!self.log_options.previous, |button| button.ghost())
                            .label("Previous")
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
                            .label("Timestamps")
                            .on_click(cx.listener(|view, _, window, cx| {
                                let options = LogOptions {
                                    timestamps: !view.log_options.timestamps,
                                    ..view.log_options.clone()
                                };
                                view.set_log_options(options, window, cx);
                            })),
                    )
                    .child(
                        Button::new("copy-logs")
                            .xsmall()
                            .ghost()
                            .label("Copy")
                            .on_click(cx.listener(|view, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    view.logs.to_text(),
                                ));
                            })),
                    )
                    .child(div().text_color(cx.theme().muted_foreground).child(
                        match (dropped, &self.log_status) {
                            // Saying so matters: what is shown starts
                            // mid-stream, and nothing else would say that.
                            (0, LogStatus::Ended) => format!("{count} lines · ended"),
                            (0, _) => format!("{count} lines"),
                            (dropped, _) => {
                                format!("{count} lines · {dropped} older dropped")
                            }
                        },
                    )),
            )
            .child(div().flex_1().overflow_hidden().child(body))
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
                    .overflow_scroll()
                    .p_3()
                    .child(
                        v_flex()
                            .gap_2()
                            .children(output.note.clone().map(|note| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().tone(Tone::Warning))
                                    .child(note)
                            }))
                            .child(div().font_family("monospace").text_xs().child(
                                if text.is_empty() {
                                    "(no output)".to_string()
                                } else {
                                    text
                                },
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
                            .label(if running { "Running…" } else { "Run" })
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

        h_flex()
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
            .child(message.into())
            .into_any_element()
    }
}

impl Render for PodToolsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            PodToolTab::Logs => self.render_logs(cx),
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
