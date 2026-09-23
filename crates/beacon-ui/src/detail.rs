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
use beacon_kube::{ClusterSession, DynamicObject, Kind, ObjectRef, ResourceStore, WatchKey};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
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
}

impl DetailTab {
    const ALL: [Self; 3] = [Self::Overview, Self::Yaml, Self::Events];

    fn label(&self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Yaml => "YAML",
            Self::Events => "Events",
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

pub struct DetailView {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    target: ObjectRef,
    /// The slimmed copy from the table's store, replaced as the watch updates
    /// it so that Overview stays live.
    object: Arc<DynamicObject>,

    tab: DetailTab,
    yaml: Yaml,
    yaml_editor: Entity<EditorState>,

    events: ResourceStore,
    events_listed: bool,

    now: Timestamp,

    _yaml_task: Option<Task<()>>,
    _events_task: Option<Task<()>>,
    _clock: Task<()>,
}

/// Emitted when the panel wants to be closed.
pub struct DetailClosed;

impl EventEmitter<DetailClosed> for DetailView {}

impl DetailView {
    pub fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        object: Arc<DynamicObject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let yaml_editor = cx.new(|cx| EditorState::new(window, cx).language("yaml"));

        let mut this = Self {
            target: ObjectRef::of(&object),
            session,
            kind,
            object,
            tab: DetailTab::Overview,
            yaml: Yaml::Unopened,
            yaml_editor,
            events: ResourceStore::new(),
            events_listed: false,
            now: Timestamp::now(),
            _yaml_task: None,
            _events_task: None,
            _clock: Task::ready(()),
        };

        this.watch_events(window, cx);
        this.start_clock(cx);
        this
    }

    pub fn target(&self) -> &ObjectRef {
        &self.target
    }

    /// The object this panel is about, as the table now knows it.
    ///
    /// Replacing it rather than rebuilding the panel is what keeps the tab, the
    /// scroll position and the events watch across an update -- and a busy
    /// object updates several times a second.
    pub fn refresh(&mut self, object: Arc<DynamicObject>, cx: &mut Context<Self>) {
        self.object = object;
        cx.notify();
    }

    fn select(&mut self, tab: DetailTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        if tab == DetailTab::Yaml && matches!(self.yaml, Yaml::Unopened) {
            self.load_yaml(window, cx);
        }
        cx.notify();
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
                            DetailTab::ALL
                                .iter()
                                .position(|tab| *tab == self.tab)
                                .unwrap_or(0),
                        )
                        .children(DetailTab::ALL.map(|tab| Tab::new().child(tab.label())))
                        .on_click(cx.listener(|view, index: &usize, window, cx| {
                            if let Some(tab) = DetailTab::ALL.get(*index).copied() {
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
            Yaml::Ready => div()
                .size_full()
                .p_2()
                .child(
                    // Read-only for now: editing goes through Server-Side
                    // Apply, which arrives with the write operations in M4.
                    Editor::new(&self.yaml_editor)
                        .readonly(true)
                        .bordered(false)
                        .h(relative(1.)),
                )
                .into_any_element(),
        }
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
