//! The detail panel: everything about one object.
//!
//! Three tabs, and they answer three different questions. Overview is what the
//! object *is*, read out of the copy the table already has. YAML is what the
//! API server would hand you, which means fetching it again -- the store holds
//! slimmed objects and the whole point of this tab is the parts that were
//! stripped. Events is what the cluster has *said* about it, which is almost
//! always where the answer is when something is wrong.

use std::sync::Arc;

use beacon_columns::{EventSummary, Timestamp, format_age, format_duration};
use beacon_kube::{
    Applied, ClusterSession, Conflict, DynamicObject, Kind, LogBuffer, LogEvent, LogOptions,
    ObjectRef, Operation, ResourceStore, Rules, WatchKey,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::bridge::{Bridge, drain_into};
use crate::theme::{BeaconTheme as _, Tone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Overview,
    Yaml,
    Events,
    Logs,
}

impl DetailTab {
    /// The tabs for a kind. Logs are a Pod idea; offering them on a ConfigMap
    /// would be a tab that can only ever say "not applicable".
    fn for_kind(kind: &Kind) -> Vec<Self> {
        let mut tabs = vec![Self::Overview, Self::Yaml, Self::Events];
        if kind.resource.kind == "Pod" && kind.resource.group.is_empty() {
            tabs.push(Self::Logs);
        }
        tabs
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Yaml => "YAML",
            Self::Events => "Events",
            Self::Logs => "Logs",
        }
    }
}

/// What the YAML tab has to show.
enum Yaml {
    /// Nobody has opened the tab yet, so nothing has been fetched.
    Unopened,
    Loading,
    Ready,
    Failed(String),
}

/// Where an apply got to.
enum Apply {
    Idle,
    Running,
    /// Somebody else owns fields this apply would change. Nothing was written.
    Refused(Box<Conflict>),
    Failed(String),
    Done,
}

pub struct DetailView {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    target: ObjectRef,
    /// The slimmed copy from the table's store, replaced as the watch updates
    /// it so that Overview stays live.
    object: Arc<DynamicObject>,

    tabs: Vec<DetailTab>,
    tab: DetailTab,

    yaml: Yaml,
    yaml_editor: Entity<EditorState>,
    apply: Apply,

    events: ResourceStore,
    events_listed: bool,

    logs: LogBuffer,
    log_options: LogOptions,
    log_status: LogStatus,
    log_scroll: UniformListScrollHandle,

    /// What this user may do here. `None` until the answer arrives; see
    /// [`crate::actions`].
    rules: Option<Arc<Rules>>,

    now: Timestamp,

    _yaml_task: Option<Task<()>>,
    _apply_task: Option<Task<()>>,
    _events_task: Option<Task<()>>,
    _logs_task: Option<Task<()>>,
    _clock: Task<()>,
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

/// Emitted when the panel wants to be closed.
pub struct DetailClosed;

impl EventEmitter<DetailClosed> for DetailView {}

impl DetailView {
    pub fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        object: Arc<DynamicObject>,
        rules: Option<Arc<Rules>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let yaml_editor = cx.new(|cx| EditorState::new(window, cx).language("yaml"));

        let mut this = Self {
            target: ObjectRef::of(&object),
            session,
            tabs: DetailTab::for_kind(&kind),
            kind,
            object,
            tab: DetailTab::Overview,
            yaml: Yaml::Unopened,
            yaml_editor,
            apply: Apply::Idle,
            events: ResourceStore::new(),
            events_listed: false,
            logs: LogBuffer::new(),
            log_options: LogOptions::default(),
            log_status: LogStatus::Unopened,
            log_scroll: UniformListScrollHandle::new(),
            rules,
            now: Timestamp::now(),
            _yaml_task: None,
            _apply_task: None,
            _events_task: None,
            _logs_task: None,
            _clock: Task::ready(()),
        };

        this.watch_events(window, cx);
        this.start_clock(cx);
        this
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

    pub fn target(&self) -> &ObjectRef {
        &self.target
    }

    /// The object this panel is about, as the table now knows it.
    ///
    /// Replacing it rather than rebuilding the panel is what keeps the tab, the
    /// scroll position and the events watch across an update -- and a busy
    /// object updates several times a second.
    pub fn refresh(
        &mut self,
        object: Arc<DynamicObject>,
        rules: Option<Arc<Rules>>,
        cx: &mut Context<Self>,
    ) {
        self.object = object;
        self.rules = rules;
        cx.notify();
    }

    fn select(&mut self, tab: DetailTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        match tab {
            DetailTab::Yaml if matches!(self.yaml, Yaml::Unopened) => self.load_yaml(window, cx),
            DetailTab::Logs if self.log_status == LogStatus::Unopened => {
                self.follow_logs(window, cx)
            }
            _ => {}
        }
        cx.notify();
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

    /// Sends the edited YAML back with Server-Side Apply.
    ///
    /// `force` takes ownership of the fields another manager holds. It is only
    /// ever reached from the conflict view, after the refusal has been read.
    pub fn apply(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let yaml = self.yaml_editor.read(cx).value().to_string();

        let object: Value = match serde_saphyr::from_str(&yaml) {
            Ok(object) => object,
            Err(error) => {
                self.apply = Apply::Failed(format!("This is not valid YAML: {error}"));
                cx.notify();
                return;
            }
        };

        self.apply = Apply::Running;
        cx.notify();

        let session = self.session.clone();
        let resource = self.kind.resource.clone();
        let target = self.target.clone();

        let applying = Bridge::global(cx).run(async move {
            session
                .run(
                    Operation::Apply,
                    resource,
                    target.namespace.clone(),
                    target.name.clone(),
                    Some(object),
                    force,
                )
                .await
        });

        self._apply_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = applying.await;

            let _ = this.update_in(cx, |view, window, cx| {
                view.apply = match result {
                    Ok(Ok(Applied::Ok(_))) => {
                        // Re-read rather than trust the echo: defaulting and
                        // admission webhooks both change what was sent.
                        view.yaml = Yaml::Unopened;
                        view.load_yaml(window, cx);
                        Apply::Done
                    }
                    Ok(Ok(Applied::Conflict(conflict))) => Apply::Refused(Box::new(conflict)),
                    Ok(Err(error)) => Apply::Failed(error.to_string()),
                    Err(error) => Apply::Failed(error.to_string()),
                };
                cx.notify();
            });
        }));
    }

    /// Fetches the object in full and renders it as YAML.
    fn load_yaml(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.yaml = Yaml::Loading;

        let session = self.session.clone();
        let resource = self.kind.resource.clone();
        let target = self.target.clone();

        let fetching = Bridge::global(cx).run(async move {
            let object = session
                .get_object(resource, target.namespace.clone(), target.name.clone())
                .await
                .map_err(|error| error.to_string())?;

            // Serialising is not free on a large object, and it is pure CPU
            // with no reason to be on the foreground thread.
            serde_saphyr::to_string(&object).map_err(|error| error.to_string())
        });

        self._yaml_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = fetching.await;

            let _ = this.update_in(cx, |view, window, cx| {
                match result {
                    Ok(Ok(yaml)) => {
                        view.yaml_editor
                            .update(cx, |editor, cx| editor.set_value(yaml, window, cx));
                        view.yaml = Yaml::Ready;
                    }
                    Ok(Err(error)) => view.yaml = Yaml::Failed(error),
                    Err(error) => view.yaml = Yaml::Failed(error.to_string()),
                }
                cx.notify();
            });
        }));
    }

    /// Follows everything the cluster says about this object.
    fn watch_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(uid) = self.object.metadata.uid.clone() else {
            // Without a UID there is nothing to key on, and matching by name
            // would show events about whatever held the name before.
            return;
        };

        let key = WatchKey::events_about(&uid, self.target.namespace.clone());
        let subscription = self.session.subscribe(key);

        self._events_task = Some(drain_into(
            cx,
            subscription,
            |view, batch, _window, cx| {
                view.events_listed = true;
                if view.events.apply_batch(batch) {
                    cx.notify();
                }
            },
            window,
        ));
    }

    fn start_clock(&mut self, cx: &mut Context<Self>) {
        self._clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let updated = this.update(cx, |view, cx| {
                    view.now = Timestamp::now();
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });
    }

    // MARK: rendering

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let subtitle = match &self.target.namespace {
            Some(namespace) => format!("{} · {namespace}", self.kind.display_name()),
            None => self.kind.display_name(),
        };

        h_flex()
            .w_full()
            .px_3()
            .py_1p5()
            .gap_3()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .overflow_hidden()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_sm()
                            .child(self.target.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(subtitle),
                    ),
            )
            .child(
                h_flex().gap_2().items_center().child(
                    TabBar::new("detail-tabs")
                        .selected_index(
                            self.tabs
                                .iter()
                                .position(|tab| *tab == self.tab)
                                .unwrap_or(0),
                        )
                        .children(
                            self.tabs
                                .iter()
                                .map(|tab| Tab::new().child(tab.label()))
                                .collect::<Vec<_>>(),
                        )
                        .on_click(cx.listener(|view, index: &usize, window, cx| {
                            if let Some(tab) = view.tabs.get(*index).copied() {
                                view.select(tab, window, cx);
                            }
                        })),
                ),
            )
    }

    fn render_overview(&self, cx: &mut Context<Self>) -> AnyElement {
        let metadata = &self.object.metadata;

        let mut sections = v_flex().gap_4().p_3().w_full();

        let created = metadata
            .creation_timestamp
            .as_ref()
            .map(|at| format!("{} ago ({})", format_age(at, self.now), at.0));

        sections = sections.child(self.section(
            "Metadata",
            vec![
                ("Name", Some(self.target.name.clone())),
                ("Namespace", self.target.namespace.clone()),
                ("Created", created),
                ("UID", metadata.uid.clone()),
                ("Owner", self.owner()),
            ],
            cx,
        ));

        if let Some(labels) = &metadata.labels
            && !labels.is_empty()
        {
            sections = sections.child(self.chips("Labels", labels, cx));
        }
        if let Some(annotations) = &metadata.annotations
            && !annotations.is_empty()
        {
            sections = sections.child(self.chips("Annotations", annotations, cx));
        }

        if let Some(containers) = self.containers() {
            sections = sections.child(self.container_section(containers, cx));
        }

        let status = scalars(self.object.data.get("status"));
        if !status.is_empty() {
            sections = sections.child(
                self.section(
                    "Status",
                    status
                        .into_iter()
                        .map(|(key, value)| (key, Some(value)))
                        .collect(),
                    cx,
                ),
            );
        }

        div()
            .id("overview")
            .size_full()
            .overflow_y_scroll()
            .child(sections)
            .into_any_element()
    }

    /// A titled block of label/value rows. Absent values are shown rather than
    /// hidden: "this object has no owner" is information.
    fn section(
        &self,
        title: &'static str,
        rows: Vec<(impl Into<SharedString>, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_1()
            .w_full()
            .child(self.heading(title, cx))
            .children(rows.into_iter().map(|(label, value)| {
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_start()
                    .text_sm()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .text_color(cx.theme().muted_foreground)
                            .child(label.into()),
                    )
                    .child(match value {
                        Some(value) => div().flex_1().child(value),
                        None => div()
                            .flex_1()
                            .text_color(cx.theme().muted_foreground)
                            .child("<none>"),
                    })
            }))
    }

    fn heading(&self, title: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().muted_foreground)
            .child(title.to_uppercase())
    }

    fn chips(
        &self,
        title: &'static str,
        entries: &std::collections::BTreeMap<String, String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_1p5()
            .w_full()
            .child(self.heading(title, cx))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1p5()
                    .children(entries.iter().map(|(key, value)| {
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .bg(cx.theme().muted)
                            .text_xs()
                            .child(if value.is_empty() {
                                key.clone()
                            } else {
                                format!("{key}={value}")
                            })
                    })),
            )
    }

    fn container_section(
        &self,
        containers: Vec<ContainerLine>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_1p5()
            .w_full()
            .child(self.heading("Containers", cx))
            .children(containers.into_iter().map(|container| {
                let tone = if container.ready {
                    Tone::Healthy
                } else {
                    crate::status::tone(&container.state)
                };

                v_flex()
                    .w_full()
                    .gap_0p5()
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().muted.opacity(0.5))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_sm()
                            .child(div().font_weight(FontWeight::MEDIUM).child(container.name))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().tone(tone))
                                    .child(container.state),
                            )
                            .when(container.restarts > 0, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("{} restarts", container.restarts)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(container.image),
                    )
            }))
    }

    fn render_yaml(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.yaml {
            Yaml::Unopened | Yaml::Loading => {
                self.notice("Fetching the object…", Tone::Progressing, cx)
            }
            Yaml::Failed(error) => self.notice(error.clone(), Tone::Critical, cx),
            Yaml::Ready => {
                let may_apply = crate::actions::may_apply(&self.kind, self.rules.as_deref());

                v_flex()
                    .size_full()
                    .child(
                        div().flex_1().overflow_hidden().p_2().child(
                            Editor::new(&self.yaml_editor)
                                .readonly(!may_apply)
                                .bordered(false)
                                .h(relative(1.)),
                        ),
                    )
                    .child(self.render_apply_bar(may_apply, cx))
                    .into_any_element()
            }
        }
    }

    /// The bar under the editor: what applying would do, and what it did.
    fn render_apply_bar(&self, may_apply: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let running = matches!(self.apply, Apply::Running);

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_3()
            .items_start()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().child(self.render_apply_status(cx)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .when(matches!(self.apply, Apply::Refused(_)), |this| {
                        this.child(
                            Button::new("force-apply")
                                .danger()
                                .small()
                                .label("Apply anyway")
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.apply(true, window, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new("apply")
                            .primary()
                            .small()
                            .label(if running { "Applying…" } else { "Apply" })
                            .disabled(!may_apply || running)
                            .on_click(
                                cx.listener(|view, _, window, cx| view.apply(false, window, cx)),
                            ),
                    ),
            )
    }

    /// What the last apply said.
    ///
    /// A conflict gets the most room: the field list and who owns it is the
    /// whole decision, and "apply anyway" should not be taken without it.
    fn render_apply_status(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.apply {
            Apply::Idle => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!(
                    "Server-Side Apply as field manager “{}”",
                    beacon_kube::ops::FIELD_MANAGER
                ))
                .into_any_element(),
            Apply::Running => div()
                .text_xs()
                .text_color(cx.theme().tone(Tone::Progressing))
                .child("Applying…")
                .into_any_element(),
            Apply::Done => div()
                .text_xs()
                .text_color(cx.theme().tone(Tone::Healthy))
                .child("Applied.")
                .into_any_element(),
            Apply::Failed(error) => div()
                .text_xs()
                .text_color(cx.theme().tone(Tone::Critical))
                .child(error.clone())
                .into_any_element(),
            Apply::Refused(conflict) => {
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(cx.theme().tone(Tone::Warning))
                            .child(format!("Not applied — {}", conflict.summary())),
                    )
                    .children(conflict.fields.iter().map(|field| {
                        let mine = self.value_at(field, cx);
                        h_flex()
                            .gap_2()
                            .items_baseline()
                            .text_xs()
                            .child(
                                div()
                                    .font_family("monospace")
                                    .text_color(cx.theme().foreground)
                                    .child(field.clone()),
                            )
                            .child(div().text_color(cx.theme().muted_foreground).child(
                                match mine {
                                    Some(value) => format!("yours: {value}"),
                                    None => "yours: (removed)".to_string(),
                                },
                            ))
                    }))
                    .into_any_element()
            }
        }
    }

    /// The value the edited YAML has at a conflicting field path.
    ///
    /// The API server writes paths like `.spec.replicas`, which the column
    /// evaluator already reads. It also writes list keys as
    /// `containers[name="app"]`, which it does not -- those resolve to nothing
    /// and the row shows the path alone, which is still the useful half.
    fn value_at(&self, field: &str, cx: &App) -> Option<String> {
        let edited = self.edited(cx)?;
        let found = beacon_columns::path::evaluate(field, &edited);
        match found.first()? {
            Value::String(text) => Some(text.clone()),
            other => Some(other.to_string()),
        }
    }

    fn edited(&self, cx: &App) -> Option<Value> {
        // Not cached: this runs only while a conflict is on screen.
        serde_saphyr::from_str(&self.yaml_editor.read(cx).value()).ok()
    }

    fn render_events(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.events_listed {
            return self.notice("Looking for events…", Tone::Progressing, cx);
        }

        let mut events: Vec<(Option<Timestamp>, EventSummary)> = self
            .events
            .iter()
            .map(|(_, object)| {
                let summary = EventSummary::read(&object.data, self.now);
                (
                    object.metadata.creation_timestamp.as_ref().map(|at| at.0),
                    summary,
                )
            })
            .collect();

        if events.is_empty() {
            // Kubernetes drops events after an hour by default, so this is the
            // ordinary state for a healthy object rather than a failure.
            return self.notice(
                "No events. Kubernetes keeps them for about an hour.",
                Tone::Unknown,
                cx,
            );
        }

        // Most recent first, which is the order somebody diagnosing reads in.
        events.sort_by(|(left, _), (right, _)| right.cmp(left));

        div()
            .id("events")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .w_full()
                    .children(events.into_iter().map(|(_, event)| {
                        let tone = if event.is_warning() {
                            Tone::Warning
                        } else {
                            Tone::Unknown
                        };

                        v_flex()
                            .w_full()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_baseline()
                                    .text_xs()
                                    .child(
                                        div()
                                            .text_color(cx.theme().tone(tone))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(event.reason),
                                    )
                                    .child(
                                        div()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{} ago", event.last_seen)),
                                    )
                                    .when(event.count > 1, |this| {
                                        this.child(
                                            div()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!("×{}", event.count)),
                                        )
                                    }),
                            )
                            .child(div().text_sm().child(event.message))
                    })),
            )
            .into_any_element()
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

    fn notice(
        &self,
        message: impl Into<SharedString>,
        tone: Tone,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .size_full()
            .p_6()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().tone(tone))
            .child(message.into())
            .into_any_element()
    }

    // MARK: reading the object

    fn owner(&self) -> Option<String> {
        let owner = self.object.metadata.owner_references.as_ref()?.first()?;
        Some(format!("{}/{}", owner.kind, owner.name))
    }

    /// The containers of a Pod, merged with what their statuses say.
    ///
    /// `None` for anything that is not a Pod, which is what keeps the section
    /// out of a Deployment's overview.
    fn containers(&self) -> Option<Vec<ContainerLine>> {
        if self.kind.resource.kind != "Pod" || !self.kind.resource.group.is_empty() {
            return None;
        }

        let spec = self.object.data.get("spec")?;
        let specs = spec.get("containers")?.as_array()?;
        let statuses = self
            .object
            .data
            .get("status")
            .and_then(|status| status.get("containerStatuses"))
            .and_then(Value::as_array);

        Some(
            specs
                .iter()
                .map(|container| {
                    let name = container
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let status = statuses.and_then(|statuses| {
                        statuses
                            .iter()
                            .find(|status| status.get("name").and_then(Value::as_str) == Some(name))
                    });

                    ContainerLine {
                        name: name.to_string(),
                        image: container
                            .get("image")
                            .and_then(Value::as_str)
                            .unwrap_or("<no image>")
                            .to_string(),
                        ready: status
                            .and_then(|status| status.get("ready"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        restarts: status
                            .and_then(|status| status.get("restartCount"))
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                        state: status
                            .map(container_state)
                            .unwrap_or_else(|| "Pending".into()),
                    }
                })
                .collect(),
        )
    }
}

struct ContainerLine {
    name: String,
    image: String,
    ready: bool,
    restarts: i64,
    state: String,
}

/// The one word a container's state reduces to, with the waiting or
/// termination reason when there is one -- `CrashLoopBackOff` says more than
/// `Waiting`.
fn container_state(status: &Value) -> String {
    let Some(state) = status.get("state").and_then(Value::as_object) else {
        return "Unknown".into();
    };

    for phase in ["waiting", "terminated"] {
        if let Some(detail) = state.get(phase) {
            return match detail.get("reason").and_then(Value::as_str) {
                Some(reason) if !reason.is_empty() => reason.to_string(),
                _ => phase.to_string(),
            };
        }
    }

    if let Some(running) = state.get("running") {
        return match running.get("startedAt").and_then(Value::as_str) {
            Some(started) => match started.parse::<Timestamp>() {
                Ok(started) => format!(
                    "Running for {}",
                    format_duration(Timestamp::now().duration_since(started).as_secs())
                ),
                Err(_) => "Running".into(),
            },
            None => "Running".into(),
        };
    }

    "Unknown".into()
}

/// The scalar fields of an object, flattened one level.
///
/// Nested objects and arrays are left to the YAML tab; what is useful here is
/// the handful of numbers and strings that answer "what is it doing" --
/// replica counts, a phase, a cluster IP.
fn scalars(node: Option<&Value>) -> Vec<(String, String)> {
    let Some(Value::Object(fields)) = node else {
        return Vec::new();
    };

    fields
        .iter()
        .filter_map(|(key, value)| {
            let rendered = match value {
                Value::String(text) if !text.is_empty() => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                _ => return None,
            };
            Some((key.clone(), rendered))
        })
        .collect()
}

impl Render for DetailView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            DetailTab::Overview => self.render_overview(cx),
            DetailTab::Yaml => self.render_yaml(cx),
            DetailTab::Events => self.render_events(cx),
            DetailTab::Logs => self.render_logs(cx),
        };

        v_flex()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(div().flex_1().overflow_hidden().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::{container_state, scalars};
    use serde_json::json;

    /// `Waiting` is never the useful word; the reason is.
    #[test]
    fn a_container_state_prefers_its_reason() {
        assert_eq!(
            container_state(&json!({ "state": { "waiting": { "reason": "CrashLoopBackOff" } } })),
            "CrashLoopBackOff"
        );
        assert_eq!(
            container_state(&json!({ "state": { "terminated": { "reason": "Completed" } } })),
            "Completed"
        );
        assert_eq!(
            container_state(&json!({ "state": { "waiting": {} } })),
            "waiting"
        );
    }

    #[test]
    fn a_container_with_no_state_is_unknown() {
        assert_eq!(container_state(&json!({})), "Unknown");
        assert_eq!(container_state(&json!({ "state": {} })), "Unknown");
    }

    /// The Status section is a glance, not a dump: nested structure belongs to
    /// the YAML tab.
    #[test]
    fn only_scalars_reach_the_status_section() {
        let status = json!({
            "phase": "Running",
            "replicas": 3,
            "paused": true,
            "empty": "",
            "conditions": [{ "type": "Ready" }],
            "loadBalancer": { "ingress": [] }
        });

        let mut found = scalars(Some(&status));
        found.sort();
        assert_eq!(
            found,
            [
                ("paused".to_string(), "true".to_string()),
                ("phase".to_string(), "Running".to_string()),
                ("replicas".to_string(), "3".to_string()),
            ]
        );
    }

    #[test]
    fn a_missing_status_is_no_rows() {
        assert!(scalars(None).is_empty());
        assert!(scalars(Some(&json!("not an object"))).is_empty());
    }
}
