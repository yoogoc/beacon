//! The detail panel: everything about one object.
//!
//! Three tabs, and they answer three different questions. Overview is what the
//! object *is*, read out of the copy the table already has. YAML is what the
//! API server would hand you, which means fetching it again -- the store holds
//! slimmed objects and the whole point of this tab is the parts that were
//! stripped. Events is what the cluster has *said* about it, which is almost
//! always where the answer is when something is wrong.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::copyable_text::copyable_text;
use beacon_columns::{EventSummary, Timestamp, format_age, format_duration};
use beacon_kube::{
    Applied, ClusterSession, Conflict, DataEntry, DynamicObject, Kind, ObjectRef, Operation,
    ResourceStore, Rules, WatchKey, data,
};
use gpui_kit::base::{Link, SelectableText, TextSelection};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::bridge::{Bridge, drain_into};
use crate::theme::{BeaconTheme as _, Tone};
use crate::tls::{self, CertificateInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Overview,
    Pods,
    /// A ConfigMap's or Secret's keys, one editor each.
    Data,
    Yaml,
    Events,
}

impl DetailTab {
    /// Pod streams and terminals belong to the independent bottom panel.
    fn for_kind(kind: &Kind) -> Vec<Self> {
        let mut tabs = vec![Self::Overview];
        if kind.resource.group.is_empty() && kind.resource.kind == "Node" {
            tabs.push(Self::Pods);
        }
        if data::is_keyed(&kind.resource.group, &kind.resource.kind) {
            tabs.push(Self::Data);
        }
        tabs.extend([Self::Yaml, Self::Events]);
        tabs
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Pods => "Pods",
            Self::Data => "Data",
            Self::Yaml => "YAML",
            Self::Events => "Events",
        }
    }
}

/// One key of a ConfigMap or Secret, and the box its value is edited in.
struct DataKey {
    entry: DataEntry,
    /// `None` for a value that is not text. It is described, not edited:
    /// putting a TLS key through a text box would corrupt it on save.
    editor: Option<Entity<TextareaState>>,
}

/// What the Data tab has to show.
enum Data {
    /// Nobody has opened the tab yet, so no editors have been built.
    Unopened,
    Ready(Vec<DataKey>),
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

#[derive(Clone, Copy)]
enum MetadataGroup {
    Labels,
    Annotations,
}

impl MetadataGroup {
    fn title(self) -> &'static str {
        match self {
            Self::Labels => "Labels",
            Self::Annotations => "Annotations",
        }
    }
}

pub struct DetailView {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    target: ObjectRef,
    /// The slimmed copy from the table's store, replaced as the watch updates
    /// it so that Overview stays live.
    object: Arc<DynamicObject>,
    /// Parsed only for Secrets of type `kubernetes.io/tls`; never includes the
    /// private key.
    certificates: Option<Result<Vec<CertificateInfo>, String>>,

    tabs: Vec<DetailTab>,
    tab: DetailTab,
    labels_expanded: bool,
    annotations_expanded: bool,
    expanded_sections: BTreeSet<String>,
    overview: crate::overview::Projection,
    node_pods: Option<Entity<crate::node_pods::NodePodsView>>,
    argo_nodes: Arc<beacon_kube::argo::Nodes>,
    argo_graph: crate::argo::Graph,
    argo_graph_error: Option<String>,
    argo_loading: bool,
    argo_scope: Option<String>,
    argo_template: String,
    argo_node: Option<WeakEntity<crate::argo_node::ArgoNodeView>>,
    _argo_task: Option<Task<()>>,
    argo_runs: ResourceStore,
    argo_runs_listed: bool,
    _argo_runs_task: Option<Task<()>>,
    resolved_owners: BTreeMap<String, Vec<OwnerLink>>,
    owner_sources: Vec<OwnerLink>,
    _owners_task: Option<Task<()>>,

    yaml: Yaml,
    /// The Data tab's editors, built when it is first opened.
    data: Data,
    /// A Secret's values start covered. They are the one thing in this
    /// application that somebody might not want on screen while a colleague
    /// walks past, and an editor is not a place to keep them by default.
    revealed: bool,
    yaml_editor: Entity<EditorState>,
    apply: Apply,
    reviewing: bool,

    events: ResourceStore,
    events_listed: bool,

    /// What this user may do here. `None` until the answer arrives; see
    /// [`crate::actions`].
    rules: Option<Arc<Rules>>,

    now: Timestamp,

    _yaml_task: Option<Task<()>>,
    _apply_task: Option<Task<()>>,
    _review_task: Option<Task<()>>,
    _events_task: Option<Task<()>>,
    _clock: Task<()>,
}

/// Emitted when the panel wants to be closed.
pub struct DetailClosed;

impl EventEmitter<DetailClosed> for DetailView {}

pub(crate) struct OwnerRequested {
    pub kind: Arc<Kind>,
    pub target: ObjectRef,
}

impl EventEmitter<OwnerRequested> for DetailView {}

impl DetailView {
    pub fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        object: Arc<DynamicObject>,
        rules: Option<Arc<Rules>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let certificates = tls_certificates(&kind, &object);
        let overview = crate::overview::project(&kind.resource.group, &kind.resource.kind, &object);
        let yaml_editor = cx.new(|cx| EditorState::new(window, cx).language("yaml"));

        let mut this = Self {
            target: ObjectRef::of(&object),
            session,
            tabs: DetailTab::for_kind(&kind),
            kind,
            object,
            certificates,
            tab: DetailTab::Overview,
            labels_expanded: false,
            annotations_expanded: false,
            expanded_sections: BTreeSet::new(),
            overview,
            node_pods: None,
            argo_nodes: Arc::new(beacon_kube::argo::Nodes::new()),
            argo_graph: crate::argo::Graph::default(),
            argo_graph_error: None,
            argo_loading: false,
            argo_scope: None,
            argo_template: String::new(),
            argo_node: None,
            _argo_task: None,
            argo_runs: ResourceStore::new(),
            argo_runs_listed: false,
            _argo_runs_task: None,
            resolved_owners: BTreeMap::new(),
            owner_sources: Vec::new(),
            _owners_task: None,
            yaml: Yaml::Unopened,
            data: Data::Unopened,
            revealed: false,
            yaml_editor,
            apply: Apply::Idle,
            reviewing: false,
            events: ResourceStore::new(),
            events_listed: false,
            rules,
            now: Timestamp::now(),
            _yaml_task: None,
            _apply_task: None,
            _review_task: None,
            _events_task: None,
            _clock: Task::ready(()),
        };

        this.resolve_pod_owners(cx);
        this.load_argo(cx);
        this.watch_argo_runs(window, cx);
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
    pub fn refresh(
        &mut self,
        object: Arc<DynamicObject>,
        rules: Option<Arc<Rules>>,
        cx: &mut Context<Self>,
    ) {
        self.certificates = tls_certificates(&self.kind, &object);
        self.overview =
            crate::overview::project(&self.kind.resource.group, &self.kind.resource.kind, &object);
        let changed = !Arc::ptr_eq(&self.object, &object);
        self.object = object;
        if changed {
            self.load_argo(cx);
        }
        self.resolve_pod_owners(cx);
        self.rules = rules;
        cx.notify();
    }

    pub(crate) fn select(&mut self, tab: DetailTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.contains(&tab) {
            return;
        }
        if self.tab == tab {
            return;
        }
        // Replacing the Overview with a nested table changes the retained
        // paint tree. GPUI Fast 0.1.x can replay obsolete child ranges at
        // this boundary; draw a fresh frame when entering or leaving Pods.
        if self.tab == DetailTab::Pods || tab == DetailTab::Pods {
            window.refresh();
        }
        self.tab = tab;
        match tab {
            DetailTab::Pods if self.node_pods.is_none() => self.load_node_pods(window, cx),
            DetailTab::Data if matches!(self.data, Data::Unopened) => self.load_data(window, cx),
            DetailTab::Yaml if matches!(self.yaml, Yaml::Unopened) => self.load_yaml(window, cx),
            _ => {}
        }
        cx.notify();
    }

    fn busy(&self) -> bool {
        self.reviewing || matches!(self.apply, Apply::Running)
    }

    fn load_node_pods(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self
            .session
            .discovery()
            .kinds()
            .iter()
            .find(|kind| kind.resource.group.is_empty() && kind.resource.kind == "Pod")
            .cloned()
            .map(Arc::new)
        else {
            return;
        };
        let session = self.session.clone();
        let node = self.target.name.clone();
        let pods =
            cx.new(|cx| crate::node_pods::NodePodsView::new(session, kind, node, window, cx));
        cx.subscribe(&pods, |_, _, event: &OwnerRequested, cx| {
            cx.emit(OwnerRequested {
                kind: event.kind.clone(),
                target: event.target.clone(),
            });
        })
        .detach();
        self.node_pods = Some(pods);
    }

    fn render_node_pods(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.node_pods {
            Some(pods) => div()
                .size_full()
                .min_size_0()
                .child(pods.clone())
                .into_any_element(),
            None => div()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(copyable_text(
                    "node-pods-unavailable",
                    "The Pod resource is not available in this cluster.",
                ))
                .into_any_element(),
        }
    }

    /// Reviews the edited YAML before Server-Side Apply.
    ///
    /// `force` takes ownership of the fields another manager holds. It is only
    /// ever reached from the conflict view, after the refusal has been read.
    pub fn apply(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let yaml = self.yaml_editor.read(cx).value().to_string();

        let object = match parse(&yaml) {
            Ok(object) => object,
            Err(error) => {
                self.apply = Apply::Failed(error);
                cx.notify();
                return;
            }
        };

        self.review(object, force, window, cx);
    }

    /// Fetch fresh comparison data; the draft is frozen until confirmed or cancelled.
    fn review(&mut self, object: Value, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || !crate::actions::may_apply(&self.kind, self.rules.as_deref()) {
            return;
        }
        self.reviewing = true;
        cx.notify();
        let session = self.session.clone();
        let resource = self.kind.resource.clone();
        let target = self.target.clone();
        let preparing = Bridge::global(cx).run(async move {
            let current = session
                .get_object(resource, target.namespace, target.name)
                .await
                .map_err(|error| error.user_message())?;
            let current = serde_json::to_value(current).map_err(|error| error.to_string())?;
            crate::yaml_review::Preview::apply(current, object)
        });
        self._review_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = preparing.await;
            let _ = this.update_in(cx, |view, window, cx| {
                match result {
                    Ok(Ok(preview)) => {
                        let context = format!(
                            "{} · {} · {}",
                            view.session.id().display_name(),
                            view.kind.resource.kind,
                            view.target
                        );
                        let review =
                            crate::yaml_review::open(preview, context, false, force, window, cx);
                        cx.subscribe_in(
                            &review,
                            window,
                            move |view, _, event: &crate::yaml_review::ReviewEvent, window, cx| {
                                view.reviewing = false;
                                if let crate::yaml_review::ReviewEvent::Confirmed(object) = event {
                                    if crate::actions::may_apply(&view.kind, view.rules.as_deref())
                                    {
                                        view.send((**object).clone(), force, window, cx);
                                    } else {
                                        view.apply = Apply::Failed(
                                            "You no longer have permission to apply this resource."
                                                .into(),
                                        );
                                    }
                                }
                                cx.notify();
                            },
                        )
                        .detach();
                    }
                    Ok(Err(error)) => {
                        view.reviewing = false;
                        view.apply = Apply::Failed(error);
                    }
                    Err(error) => {
                        view.reviewing = false;
                        view.apply = Apply::Failed(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Applies one object and reports what came back.
    ///
    /// Shared by the YAML tab and the Data tab so that a conflict, a refusal
    /// and a webhook's rewrite all read the same whichever one you edited in.
    fn send(&mut self, object: Value, force: bool, window: &mut Window, cx: &mut Context<Self>) {
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
                        view.data = Data::Unopened;
                        match view.tab {
                            DetailTab::Data => view.load_data(window, cx),
                            _ => view.load_yaml(window, cx),
                        }
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

    /// Rewrites what is in the editor in the form the pane itself produces.
    ///
    /// Useful twice over. An edited or pasted manifest comes back with block
    /// indentation instead of whatever flow style it arrived in, and because
    /// this is the same parse that `apply` does, pressing it answers "is this
    /// even valid YAML" without writing to the cluster to find out.
    pub fn format(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let yaml = self.yaml_editor.read(cx).value().to_string();

        match reformat(&yaml) {
            Ok(formatted) => {
                if formatted != yaml {
                    self.yaml_editor
                        .update(cx, |editor, cx| editor.set_value(formatted, window, cx));
                }
                // Clear a parse error this may just have fixed, but leave a
                // conflict alone: reformatting does not change the object, so
                // what the API server refused it would still refuse.
                if matches!(self.apply, Apply::Failed(_)) {
                    self.apply = Apply::Idle;
                }
            }
            Err(error) => self.apply = Apply::Failed(error),
        }

        cx.notify();
    }

    /// Builds an editor per key from the object the table already holds.
    ///
    /// No fetch: unlike the YAML tab, nothing here was slimmed away -- `data`
    /// survives the store untouched, and the only thing stripped is
    /// `managedFields`.
    fn load_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = data::read(&self.object, self.kind.resource.kind == "Secret")
            .into_iter()
            .map(|entry| {
                let editor = entry.text.clone().map(|text| {
                    cx.new(|cx| {
                        let mut state = TextareaState::new(window, cx).auto_grow(1, 12);
                        state.set_value(text, window, cx);
                        state
                    })
                });
                DataKey { entry, editor }
            })
            .collect();

        self.data = Data::Ready(keys);
        cx.notify();
    }

    /// Writes the edited values back into the object and applies it.
    ///
    /// The whole object goes, not a `data`-only patch. That is what the YAML
    /// tab does, it keeps the two paths on one set of Server-Side Apply
    /// semantics, and it means the values nobody edited -- a binary key, a
    /// label -- travel back exactly as they arrived rather than depending on
    /// which fields this field manager happens to own.
    pub fn apply_data(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let Data::Ready(keys) = &self.data else {
            return;
        };

        let secret = self.kind.resource.kind == "Secret";
        let mut object = (*self.object).clone();

        for key in keys {
            let Some(editor) = &key.editor else {
                continue;
            };
            let text = editor.read(cx).value().to_string();
            let encoded = data::encode(key.entry.field, secret, &text);
            object.data[key.entry.field.name()][&key.entry.key] = Value::String(encoded);
        }

        let object = match serde_json::to_value(&object) {
            Ok(object) => object,
            Err(error) => {
                self.apply = Apply::Failed(error.to_string());
                cx.notify();
                return;
            }
        };

        self.review(object, force, window, cx);
    }

    /// Fetches the object in full and renders it as YAML.
    fn load_yaml(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.yaml = Yaml::Loading;

        let session = self.session.clone();
        let resource = self.kind.resource.clone();
        let target = self.target.clone();

        let folding = crate::settings::store(cx)
            .read(cx)
            .preferences
            .yaml_folding(self.session.id());

        let fetching = Bridge::global(cx).run(async move {
            let object = session
                .get_object(resource, target.namespace.clone(), target.name.clone())
                .await
                .map_err(|error| error.to_string())?;

            // Serialising is not free on a large object, and it is pure CPU
            // with no reason to be on the foreground thread.
            let yaml = serde_saphyr::to_string(&object).map_err(|error| error.to_string())?;
            let folds = folding.initial_lines(&yaml);
            Ok::<_, String>((yaml, folds))
        });

        self._yaml_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = fetching.await;

            let _ = this.update_in(cx, |view, window, cx| {
                match result {
                    Ok(Ok((yaml, folds))) => {
                        view.yaml_editor.update(cx, |editor, cx| {
                            editor.set_value(yaml, window, cx);
                            editor.set_initial_folded_lines(folds, cx);
                        });
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

    fn watch_argo_runs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind.resource.group != beacon_kube::argo::GROUP
            || self.kind.resource.kind != "CronWorkflow"
        {
            return;
        }
        let Some(kind) = self.session.discovery().kinds().iter().find(|kind| {
            kind.resource.group == beacon_kube::argo::GROUP && kind.resource.kind == "Workflow"
        }) else {
            return;
        };
        let key = WatchKey::all(kind.resource.clone())
            .in_namespace(self.target.namespace.clone())
            .with_labels(format!(
                "workflows.argoproj.io/cron-workflow={}",
                self.target.name
            ));
        self._argo_runs_task = Some(drain_into(
            cx,
            self.session.subscribe(key),
            |view, batch, _, cx| {
                view.argo_runs_listed = true;
                view.argo_runs.apply_batch(batch);
                cx.notify();
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

    fn load_argo(&mut self, cx: &mut Context<Self>) {
        if self.kind.resource.group != beacon_kube::argo::GROUP {
            return;
        }
        if self.kind.resource.kind == "Workflow" {
            let object = self.object.clone();
            let scope = self.argo_scope.clone();
            self.argo_loading = true;
            self.argo_graph_error = None;
            let decoding = Bridge::global(cx).run_cancellable(async move {
                let nodes = beacon_kube::argo::nodes(&object.data)?;
                let graph = crate::argo::Graph::workflow(&object, &nodes, scope.as_deref());
                Ok::<_, String>((Arc::new(nodes), graph))
            });
            self._argo_task = Some(cx.spawn(async move |this, cx| {
                let result = decoding.result().await;
                let _ = this.update(cx, |view, cx| {
                    view.argo_loading = false;
                    match result {
                        Ok(Ok((nodes, graph))) => {
                            view.argo_nodes = nodes;
                            view.argo_graph = graph;
                        }
                        Ok(Err(error)) => {
                            view.argo_nodes = Arc::new(beacon_kube::argo::Nodes::new());
                            view.argo_graph = crate::argo::Graph::default();
                            view.argo_graph_error = Some(error);
                        }
                        Err(error) => view.argo_graph_error = Some(error.to_string()),
                    }
                    if let Some(node) = view.argo_node.as_ref().and_then(WeakEntity::upgrade) {
                        node.update(cx, |node, cx| {
                            node.refresh(view.object.clone(), view.argo_nodes.clone(), cx)
                        });
                    }
                    cx.notify();
                });
            }));
        } else if matches!(
            self.kind.resource.kind.as_str(),
            "WorkflowTemplate" | "ClusterWorkflowTemplate"
        ) {
            let spec = &self.object.data["spec"];
            if !crate::argo::array(spec, "/templates")
                .any(|template| template["name"] == self.argo_template)
            {
                self.argo_template = spec["entrypoint"]
                    .as_str()
                    .filter(|name| {
                        crate::argo::array(spec, "/templates")
                            .any(|template| template["name"] == *name)
                    })
                    .or_else(|| {
                        crate::argo::array(spec, "/templates")
                            .find_map(|template| template["name"].as_str())
                    })
                    .unwrap_or("")
                    .to_string();
            }
            self.argo_graph = crate::argo::Graph::template(spec, &self.argo_template);
            if let Some(node) = self.argo_node.as_ref().and_then(WeakEntity::upgrade) {
                node.update(cx, |node, cx| {
                    node.refresh(self.object.clone(), self.argo_nodes.clone(), cx)
                });
            }
        }
    }

    fn open_argo_node(
        &mut self,
        node: crate::argo::GraphNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let node = cx.new(|cx| {
            crate::argo_node::ArgoNodeView::new(
                self.session.clone(),
                self.object.clone(),
                node,
                self.argo_nodes.clone(),
                window,
                cx,
            )
        });
        cx.subscribe_in(
            &node,
            window,
            |view, _, event: &OwnerRequested, window, cx| {
                window.close_sheet(cx);
                view.argo_node = None;
                cx.emit(OwnerRequested {
                    kind: event.kind.clone(),
                    target: event.target.clone(),
                })
            },
        )
        .detach();
        cx.subscribe_in(
            &node,
            window,
            |view, _, event: &crate::argo_node::SubgraphRequested, window, cx| {
                window.close_sheet(cx);
                view.argo_node = None;
                view.argo_scope = Some(event.0.clone());
                view.argo_graph = crate::argo::Graph::workflow(
                    &view.object,
                    &view.argo_nodes,
                    view.argo_scope.as_deref(),
                );
                cx.notify();
            },
        )
        .detach();
        self.argo_node = Some(node.downgrade());
        let owner = cx.entity().downgrade();
        window.open_sheet(cx, move |sheet, window, _| {
            let owner = owner.clone();
            sheet
                .title("Node details")
                .size(px(560.).min(window.viewport_size().width - px(24.)))
                .resizable(true)
                .child(node.clone())
                .on_close(move |_, _, cx| {
                    let _ = owner.update(cx, |view, _| {
                        view.argo_node = None;
                    });
                })
        });
    }

    fn render_argo_graph(&self, cx: &mut Context<Self>) -> AnyElement {
        let runtime = self.kind.resource.kind == "Workflow";
        let mut view = self
            .overview_block("argo-graph-section", cx)
            .child(self.heading(
                if runtime {
                    "Execution"
                } else {
                    "Template graph"
                },
                cx,
            ));
        if runtime && self.argo_scope.is_some() {
            view = view.child(
                Button::new("main-graph")
                    .small()
                    .ghost()
                    .label("Back to workflow graph")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.argo_scope = None;
                        view.argo_graph =
                            crate::argo::Graph::workflow(&view.object, &view.argo_nodes, None);
                        cx.notify();
                    })),
            );
        }
        if !runtime {
            let templates: Vec<_> = crate::argo::array(&self.object.data, "/spec/templates")
                .filter_map(|template| template["name"].as_str().map(str::to_string))
                .collect();
            let owner = cx.entity().downgrade();
            view = view.child(
                Button::new("template-selector")
                    .small()
                    .outline()
                    .label(if self.argo_template.is_empty() {
                        "No templates".into()
                    } else {
                        self.argo_template.clone()
                    })
                    .dropdown_menu(move |menu, _, _| {
                        templates.iter().fold(menu, |menu, name| {
                            let owner = owner.clone();
                            let name = name.clone();
                            menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |view, cx| {
                                    view.argo_template = name.clone();
                                    view.argo_graph = crate::argo::Graph::template(
                                        &view.object.data["spec"],
                                        &view.argo_template,
                                    );
                                    cx.notify();
                                });
                            }))
                        })
                    }),
            );
        }
        if let Some(error) = &self.argo_graph_error {
            return view
                .child(copyable_text("argo-graph-error", error.clone()))
                .into_any_element();
        }
        if self.argo_loading && self.argo_graph.nodes.is_empty() {
            return view.child(Spinner::new().small()).into_any_element();
        }
        if self.argo_graph.nodes.is_empty() {
            return view
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(SelectableText::new(
                            "argo-graph-empty",
                            if runtime {
                                "No execution nodes yet"
                            } else {
                                "No template graph"
                            },
                        )),
                )
                .into_any_element();
        }
        let graph = &self.argo_graph;
        let edges: Vec<_> = graph
            .edges
            .iter()
            .map(|(a, b)| {
                let a = &graph.nodes[*a];
                let b = &graph.nodes[*b];
                (
                    a.x + crate::argo::CARD_WIDTH / 2.,
                    a.y + crate::argo::CARD_HEIGHT,
                    b.x + crate::argo::CARD_WIDTH / 2.,
                    b.y,
                )
            })
            .collect();
        let color = cx.theme().border;
        let canvas = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let mut path = PathBuilder::stroke(px(1.5));
                for &(x1, y1, x2, y2) in &edges {
                    let mid = (y1 + y2) / 2.;
                    path.move_to(bounds.origin + point(px(x1), px(y1)));
                    path.line_to(bounds.origin + point(px(x1), px(mid)));
                    path.line_to(bounds.origin + point(px(x2), px(mid)));
                    path.line_to(bounds.origin + point(px(x2), px(y2)));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .absolute()
        .size_full();
        let surface = div()
            .relative()
            .w(px(graph.width))
            .h(px(graph.height))
            .child(canvas)
            .children(graph.nodes.iter().map(|node| {
                let selected = node.clone();
                let phase = if node.runtime {
                    node.data["phase"].as_str().unwrap_or("Unknown")
                } else {
                    "Template"
                };
                let tone = crate::argo::phase_tone(phase);
                let subtitle = if node.runtime {
                    format!(
                        "{phase} · {}",
                        crate::argo::duration(&node.data, self.now).unwrap_or_else(|| "—".into())
                    )
                } else {
                    node.data["template"]
                        .as_str()
                        .or_else(|| {
                            node.data
                                .pointer("/templateRef/template")
                                .and_then(Value::as_str)
                        })
                        .unwrap_or("inline")
                        .into()
                };
                div()
                    .id(SharedString::from(format!("argo-node-{}", node.id)))
                    .absolute()
                    .left(px(node.x))
                    .top(px(node.y))
                    .w(px(crate::argo::CARD_WIDTH))
                    .h(px(crate::argo::CARD_HEIGHT))
                    .child(
                        Link::new("inspect-node")
                            .accessibility_label(format!("{}: {subtitle}", node.label))
                            .size_full()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background)
                            .hover(|this| this.bg(cx.theme().muted))
                            .cursor_pointer()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div().size(px(7.)).rounded_full().bg(cx.theme().tone(tone)),
                                    )
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .truncate()
                                                    .child(node.label.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .truncate()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(subtitle),
                                            ),
                                    ),
                            )
                            .on_activate(cx.listener(move |view, _, window, cx| {
                                view.open_argo_node(selected.clone(), window, cx)
                            })),
                    )
            }));
        view.child(
            div()
                .id("argo-execution-graph")
                .w_full()
                .max_h(px(520.))
                .overflow_scroll()
                .child(surface),
        )
        .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let subtitle = match &self.target.namespace {
            Some(namespace) => format!("{} · {namespace}", self.kind.display_name()),
            None => self.kind.display_name(),
        };

        v_flex()
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_start()
                    .child(
                        div()
                            .p_2()
                            .rounded_md()
                            .bg(cx.theme().muted.opacity(0.5))
                            .child(
                                crate::icons::resource(
                                    &self.kind.resource.group,
                                    &self.kind.resource.kind,
                                )
                                .size_4()
                                .text_color(cx.theme().muted_foreground),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                                SelectableText::new("detail-name", self.target.name.clone()),
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SelectableText::new("detail-subtitle", subtitle)),
                            ),
                    )
                    .child(
                        Button::new("close-detail")
                            .xsmall()
                            .ghost()
                            .label("×")
                            .tooltip("Close the details panel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DetailClosed))),
                    ),
            )
            .child(
                TabBar::new("detail-tabs")
                    .segmented()
                    .w_full()
                    .small()
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
            )
    }

    fn render_overview(&self, cx: &mut Context<Self>) -> AnyElement {
        let metadata = &self.object.metadata;
        let summary =
            if beacon_kube::argo::rank(&self.kind.resource.group, &self.kind.resource.kind)
                .is_some()
            {
                crate::argo::summary(
                    &self.kind.resource.kind,
                    &self.object,
                    &self.argo_nodes,
                    self.now,
                )
            } else {
                crate::overview::summary(
                    &self.kind.resource.group,
                    &self.kind.resource.kind,
                    &self.object,
                    self.now,
                )
            };
        let mut content = v_flex()
            .px_4()
            .pb_4()
            .w_full()
            .min_w_0()
            .child(self.render_summary(summary, cx));
        if self.kind.resource.group == beacon_kube::argo::GROUP
            && matches!(
                self.kind.resource.kind.as_str(),
                "Workflow" | "WorkflowTemplate" | "ClusterWorkflowTemplate"
            )
        {
            content = content.child(self.render_argo_graph(cx));
        }
        if self.kind.resource.group == beacon_kube::argo::GROUP
            && self.kind.resource.kind == "CronWorkflow"
        {
            let group = crate::argo::recent_runs(&self.object, self.argo_runs.snapshot(), self.now);
            let body = if self.argo_runs_listed {
                self.overview_table(group.table.as_ref().expect("recent runs table"), cx)
            } else {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(copyable_text(
                        "argo-runs-loading",
                        "Waiting for workflow history…",
                    ))
                    .into_any_element()
            };
            content = content.child(
                self.overview_block("argo-recent-runs", cx)
                    .child(self.heading("Recent runs", cx))
                    .child(body),
            );
        }
        for (index, group) in self.overview.groups.iter().enumerate() {
            let mut fields = Vec::new();
            if group.owner {
                fields.push(
                    div()
                        .flex_1()
                        .flex_basis(px(190.))
                        .min_w_0()
                        .child(self.render_owners(cx))
                        .into_any_element(),
                );
            }
            fields.extend(
                group
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(index, (label, cell))| {
                        self.field(index, label, cell, cx).into_any_element()
                    }),
            );
            let fields = self.field_grid(fields);
            let body = v_flex()
                .w_full()
                .gap_3()
                .child(fields)
                .when_some(group.table.as_ref(), |this, table| {
                    this.child(self.overview_table(table, cx))
                });
            let block = self.overview_block(("group", index), cx);
            content = content.child(if group.collapsed {
                block.child(self.disclosure(
                    &group.title,
                    &format!("argo-group-{index}"),
                    body,
                    false,
                    cx,
                ))
            } else {
                block
                    .child(self.heading(group.title.clone(), cx))
                    .child(body)
            });
        }
        if let Some(containers) = self.containers()
            && !containers.is_empty()
        {
            content = content.child(
                self.overview_block("containers-section", cx)
                    .child(self.container_section(containers, cx)),
            );
        }
        if let Some(certificates) = &self.certificates {
            content = content.child(
                self.overview_block("tls-section", cx)
                    .child(self.render_certificates(certificates, cx)),
            );
        }
        if !self.overview.conditions.is_empty() {
            let fields = self.field_rows(&self.overview.conditions, cx);
            content = content.child(
                self.overview_block("conditions-section", cx)
                    .child(self.disclosure("Conditions", "conditions", fields, false, cx)),
            );
        }
        if let Some(labels) = &metadata.labels
            && !labels.is_empty()
        {
            content = content.child(
                self.overview_block("labels-section", cx)
                    .child(self.metadata_entries(MetadataGroup::Labels, labels, cx)),
            );
        }
        if let Some(annotations) = &metadata.annotations
            && !annotations.is_empty()
        {
            content = content.child(
                self.overview_block("annotations-section", cx)
                    .child(self.metadata_entries(MetadataGroup::Annotations, annotations, cx)),
            );
        }
        let created = metadata
            .creation_timestamp
            .as_ref()
            .map(|at| at.0.to_string());
        let metadata_fields = self.field_rows(
            &[
                ("Created".into(), created),
                ("UID".into(), metadata.uid.clone()),
                (
                    "Generation".into(),
                    metadata.generation.map(|v| v.to_string()),
                ),
                ("Resource version".into(), metadata.resource_version.clone()),
                (
                    "Deletion timestamp".into(),
                    metadata
                        .deletion_timestamp
                        .as_ref()
                        .map(|v| v.0.to_string()),
                ),
                (
                    "Finalizers".into(),
                    metadata
                        .finalizers
                        .as_ref()
                        .filter(|v| !v.is_empty())
                        .map(|v| v.join(", ")),
                ),
            ],
            cx,
        );
        content = content.child(
            self.overview_block("metadata-section", cx)
                .child(self.disclosure("Metadata", "metadata", metadata_fields, false, cx)),
        );
        if !self.overview.additional.is_empty() {
            let mut additional = v_flex().w_full().gap_4();
            if self.expanded_sections.contains("additional") {
                for (title, rows) in &self.overview.additional {
                    additional = additional.child(self.section(title.clone(), rows.clone(), cx));
                }
            }
            content =
                content.child(self.overview_block("additional-section", cx).child(
                    self.disclosure("Additional fields", "additional", additional, false, cx),
                ));
        }
        div()
            .id("overview")
            .size_full()
            .overflow_y_scroll()
            .child(content)
            .into_any_element()
    }

    fn overview_block(&self, id: impl Into<ElementId>, cx: &mut Context<Self>) -> Stateful<Div> {
        v_flex()
            .id(id)
            .w_full()
            .min_w_0()
            .py_4()
            .gap_3()
            .border_b_1()
            .border_color(cx.theme().border)
    }

    fn render_summary(
        &self,
        summary: crate::overview::Summary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut line = h_flex()
            .w_full()
            .flex_wrap()
            .gap_3()
            .items_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(
                h_flex()
                    .gap_1p5()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(cx.theme().tone_surface(summary.tone))
                    .text_color(cx.theme().tone(summary.tone))
                    .when(summary.health, |this| {
                        this.child(
                            div()
                                .size(px(6.))
                                .rounded_full()
                                .bg(cx.theme().tone(summary.tone)),
                        )
                    })
                    .child(SelectableText::new("summary-status", summary.label)),
            );
        for (index, hint) in summary.hints.into_iter().enumerate() {
            line = line.child(SelectableText::new(("summary-hint", index), hint));
        }
        if let Some(created) = &self.object.metadata.creation_timestamp {
            line = line.child(SelectableText::new(
                "summary-age",
                format!("Created {} ago", format_age(created, self.now)),
            ));
        }
        self.overview_block("summary", cx).child(line).when_some(
            summary.message,
            |this, message| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().tone(if summary.tone == Tone::Healthy {
                            Tone::Warning
                        } else {
                            summary.tone
                        }))
                        .child(SelectableText::new("summary-message", message)),
                )
            },
        )
    }

    /// A minimum field width lets two columns wrap to one in a narrow pane.
    fn field(
        &self,
        index: usize,
        label: &str,
        cell: &crate::overview::Cell,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        v_flex()
            .id(("field", index))
            .flex_1()
            .flex_basis(px(190.))
            .min_w_0()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label.to_string()),
            )
            .child(self.overview_value(cell, cx))
    }

    fn overview_value(&self, cell: &crate::overview::Cell, cx: &mut Context<Self>) -> AnyElement {
        let value = cell.value.clone().unwrap_or_else(|| "<none>".into());
        let reference = cell.reference.as_ref().and_then(|reference| {
            let kind = self
                .session
                .discovery()
                .kinds()
                .iter()
                .find(|kind| {
                    kind.resource.group == reference.group && kind.resource.kind == reference.kind
                })?
                .clone();
            let target = ObjectRef::new(
                if kind.namespaced {
                    self.target.namespace.clone()
                } else {
                    None
                },
                reference.name.clone(),
            );
            Some((Arc::new(kind), target))
        });
        div()
            .w_full()
            .min_w_0()
            .text_sm()
            .whitespace_normal()
            .when(cell.value.is_none(), |this| {
                this.text_color(cx.theme().muted_foreground)
            })
            .child(match reference {
                Some((kind, target)) => Link::new("open-reference")
                    .accessibility_label(value.clone())
                    .cursor_pointer()
                    .text_color(cx.theme().resource_link())
                    .hover(|this| this.underline())
                    .child(SelectableText::new("value", value))
                    .on_activate(cx.listener(move |_, event, window, cx| {
                        if matches!(event, ClickEvent::Mouse(_))
                            && !TextSelection::selected_text(window, cx).is_empty()
                        {
                            return;
                        }
                        cx.emit(OwnerRequested {
                            kind: kind.clone(),
                            target: target.clone(),
                        });
                    }))
                    .into_any_element(),
                None => SelectableText::new("value", value).into_any_element(),
            })
            .into_any_element()
    }

    fn field_grid(&self, fields: Vec<AnyElement>) -> AnyElement {
        let mut grid = v_flex().w_full().gap_3();
        let mut fields = fields.into_iter();
        // Two slots per row cap wide panels at two columns; each row can wrap.
        while let Some(first) = fields.next() {
            let mut row = h_flex()
                .w_full()
                .flex_wrap()
                .gap_x_4()
                .gap_y_3()
                .child(first);
            if let Some(second) = fields.next() {
                row = row.child(second);
            }
            grid = grid.child(row);
        }
        grid.into_any_element()
    }

    fn field_rows(&self, rows: &[(String, Option<String>)], cx: &mut Context<Self>) -> AnyElement {
        let fields = rows
            .iter()
            .enumerate()
            .map(|(index, (label, value))| {
                self.field(
                    index,
                    label,
                    &crate::overview::Cell {
                        value: value.clone(),
                        reference: None,
                    },
                    cx,
                )
                .into_any_element()
            })
            .collect();
        self.field_grid(fields)
    }

    fn overview_table(&self, table: &crate::overview::Table, cx: &mut Context<Self>) -> AnyElement {
        if table.rows.is_empty() {
            return div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(SelectableText::new("empty-table", "<none>"))
                .into_any_element();
        }
        v_flex()
            .id("overview-table")
            .w_full()
            .min_w_0()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .bg(cx.theme().muted.opacity(0.5))
                    .children(table.headers.iter().map(|label| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_1p5()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(*label)
                    })),
            )
            .children(table.rows.iter().enumerate().map(|(index, cells)| {
                h_flex()
                    .id(("table-row", index))
                    .w_full()
                    .items_start()
                    .when(index > 0, |this| {
                        this.border_t_1().border_color(cx.theme().border)
                    })
                    .children(cells.iter().enumerate().map(|(index, cell)| {
                        div()
                            .id(("table-cell", index))
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_2()
                            .child(self.overview_value(cell, cx))
                    }))
            }))
            .into_any_element()
    }

    fn disclosure(
        &self,
        title: &str,
        key: &str,
        content: impl IntoElement,
        default_open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.expanded_sections.contains(key) != default_open;
        let key = key.to_string();
        v_flex()
            .id(SharedString::from(key.clone()))
            .w_full()
            .min_w_0()
            .items_start()
            .gap_3()
            .child(
                Button::new("toggle-section")
                    .ghost()
                    .small()
                    .max_w_full()
                    .tooltip(title.to_string())
                    .toggled(expanded)
                    .icon(if expanded {
                        gpui_kit::component::IconName::ChevronDown
                    } else {
                        gpui_kit::component::IconName::ChevronRight
                    })
                    .label(title.to_string())
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if !view.expanded_sections.remove(&key) {
                            view.expanded_sections.insert(key.clone());
                        }
                        cx.notify();
                    })),
            )
            .when(expanded, |this| this.child(content))
            .into_any_element()
    }

    fn render_certificates(
        &self,
        certificates: &Result<Vec<CertificateInfo>, String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match certificates {
            Err(error) => v_flex()
                .gap_1()
                .child(self.heading("TLS Certificate", cx))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().tone(Tone::Critical))
                        .child(copyable_text("tls-error", error.clone())),
                )
                .into_any_element(),
            Ok(certificates) => v_flex()
                .w_full()
                .gap_4()
                .children(
                    certificates.iter().enumerate().map(|(index, certificate)| {
                        self.render_certificate(index, certificate, cx)
                    }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "Expiry checks time only; issuer trust and hostname are not verified.",
                        ),
                )
                .into_any_element(),
        }
    }

    fn render_certificate(
        &self,
        index: usize,
        certificate: &CertificateInfo,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let now = x509_parser::time::ASN1Time::now().timestamp();
        let expired = now >= certificate.not_after_unix;
        let pending = now < certificate.not_before_unix;
        let tone = if expired {
            Tone::Critical
        } else if pending {
            Tone::Progressing
        } else {
            Tone::Healthy
        };
        let title = if index == 0 {
            "Leaf certificate".to_string()
        } else {
            format!("Chain certificate {}", index + 1)
        };

        v_flex()
            .id(("certificate", index))
            .w_full()
            .gap_3()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .items_center()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().text_xs().text_color(cx.theme().tone(tone)).child(
                        SelectableText::new("validity", certificate.validity_at(now)),
                    )),
            )
            .child(self.section(
                "Identity",
                vec![
                    ("Common name", certificate.common_name.clone()),
                    ("Subject", Some(certificate.subject.clone())),
                    ("Issuer", Some(certificate.issuer.clone())),
                    ("Serial", Some(certificate.serial.clone())),
                    ("Version", Some(format!("X.509 v{}", certificate.version))),
                ],
                cx,
            ))
            .child(self.section(
                "Validity",
                vec![
                    ("Not before", Some(certificate.not_before.clone())),
                    ("Not after", Some(certificate.not_after.clone())),
                ],
                cx,
            ))
            .child(self.section(
                "Algorithms",
                vec![
                    ("Signature", Some(certificate.signature_algorithm.clone())),
                    ("Public key", Some(certificate.public_key_algorithm.clone())),
                    (
                        "Key size",
                        certificate.public_key_bits.map(|bits| format!("{bits} bits")),
                    ),
                ],
                cx,
            ))
            .child(
                self.disclosure(
                    &format!("Extensions · {}", certificate.extensions.len()),
                    &format!("certificate:{index}:extensions"),
                    v_flex().w_full().gap_3().children(
                        certificate
                            .extensions
                            .iter()
                            .enumerate()
                            .map(|(index, extension)| {
                                v_flex()
                                    .id(("extension", index))
                                    .w_full()
                                    .gap_1()
                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(
                                        SelectableText::new(
                                            "name",
                                            if extension.critical {
                                                format!("{} · critical", extension.name)
                                            } else {
                                                extension.name.clone()
                                            },
                                        ),
                                    ))
                                    .child(div().text_xs().child(SelectableText::new(
                                        "details",
                                        extension.details.clone(),
                                    )))
                            }),
                    ),
                    false,
                    cx,
                ),
            )
            .child(
                self.disclosure(
                    "Public key & fingerprint",
                    &format!("certificate:{index}:key"),
                    v_flex()
                        .w_full()
                        .gap_3()
                        .child(self.field_rows(
                            &[
                                ("Key details".into(), certificate.public_key_details.clone()),
                                (
                                    "Cert SHA-256".into(),
                                    Some(certificate.sha256_fingerprint.clone()),
                                ),
                            ],
                            cx,
                        ))
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .p_2()
                                .rounded_md()
                                .bg(cx.theme().muted.opacity(0.5))
                                .font_family("monospace")
                                .text_xs()
                                .child(SelectableText::new(
                                    "public-key-pem",
                                    certificate.public_key_pem.clone(),
                                )),
                        ),
                    false,
                    cx,
                ),
            )
            .into_any_element()
    }

    fn section(
        &self,
        title: impl Into<SharedString>,
        rows: Vec<(impl Into<SharedString>, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let title: SharedString = title.into();
        let rows: Vec<_> = rows
            .into_iter()
            .map(|(k, v)| (k.into().to_string(), v))
            .collect();
        v_flex()
            .id(title.clone())
            .gap_3()
            .w_full()
            .min_w_0()
            .child(self.heading(title, cx))
            .child(self.field_rows(&rows, cx))
    }

    fn heading(&self, title: impl Into<SharedString>, cx: &mut Context<Self>) -> impl IntoElement {
        let title = title.into();
        h_flex()
            .min_w_0()
            .gap_2()
            .items_center()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .child(
                crate::icons::overview(&title)
                    .size_3p5()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(SelectableText::new("heading", title)),
            )
    }

    fn metadata_entries(
        &self,
        group: MetadataGroup,
        entries: &std::collections::BTreeMap<String, String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        const PREVIEW_LIMIT: usize = 5;
        let expanded = match group {
            MetadataGroup::Labels => self.labels_expanded,
            MetadataGroup::Annotations => self.annotations_expanded,
        };
        let visible_count = if expanded {
            entries.len()
        } else {
            PREVIEW_LIMIT
        };

        v_flex()
            .gap_2()
            .w_full()
            .items_start()
            .child(
                h_flex()
                    .gap_2()
                    .child(self.heading(group.title(), cx))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(entries.len().to_string()),
                    ),
            )
            .children(entries.iter().take(visible_count).map(|(key, value)| {
                div()
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(cx.theme().muted.opacity(0.5))
                    .text_xs()
                    .whitespace_normal()
                    .child(SelectableText::new(
                        SharedString::from(format!("{}-{key}", group.title())),
                        format!("{key}={value}"),
                    ))
            }))
            .when(entries.len() > PREVIEW_LIMIT, |this| {
                this.child(
                    Button::new(SharedString::from(format!("toggle-{}", group.title())))
                        .ghost()
                        .small()
                        .text_color(cx.theme().resource_link())
                        .label(if expanded {
                            "Show less".to_string()
                        } else {
                            format!("Show {} more", entries.len() - PREVIEW_LIMIT)
                        })
                        .on_click(cx.listener(move |view, _, _, cx| {
                            let expanded = match group {
                                MetadataGroup::Labels => &mut view.labels_expanded,
                                MetadataGroup::Annotations => &mut view.annotations_expanded,
                            };
                            *expanded = !*expanded;
                            cx.notify();
                        })),
                )
            })
    }

    fn container_section(
        &self,
        containers: Vec<ContainerLine>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .w_full()
            .child(self.heading(
                if self.kind.resource.kind == "Pod" {
                    "Containers"
                } else {
                    "Pod template"
                },
                cx,
            ))
            .children(
                containers
                    .into_iter()
                    .enumerate()
                    .map(|(index, container)| {
                        let tone = if container.ready {
                            Tone::Healthy
                        } else {
                            crate::status::tone(&container.state)
                        };
                        let key = format!("container:{}{}", container.category, container.name);
                        let default_open = index == 0 && container.category.is_empty();
                        let expanded = self.expanded_sections.contains(&key) != default_open;
                        let body = v_flex()
                            .w_full()
                            .min_w_0()
                            .px_3()
                            .pb_3()
                            .gap_3()
                            .child(self.field_rows(
                                &[
                                    ("Image".into(), Some(container.image)),
                                    ("Ports".into(), container.ports),
                                ],
                                cx,
                            ))
                            .child(self.overview_table(&container.resources, cx))
                            .child(self.disclosure(
                                "Probes, environment & mounts",
                                &format!("{key}:configuration"),
                                self.field_rows(&container.fields, cx),
                                false,
                                cx,
                            ))
                            .when(!container.status_fields.is_empty(), |this| {
                                this.child(self.disclosure(
                                    "Runtime details",
                                    &format!("{key}:runtime"),
                                    self.field_rows(&container.status_fields, cx),
                                    false,
                                    cx,
                                ))
                            });
                        v_flex()
                            .id(SharedString::from(key.clone()))
                            .w_full()
                            .min_w_0()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .flex_wrap()
                                    .gap_2()
                                    .p_2()
                                    .items_center()
                                    .child(
                                        Button::new("toggle-container")
                                            .ghost()
                                            .xsmall()
                                            .toggled(expanded)
                                            .icon(if expanded {
                                                gpui_kit::component::IconName::ChevronDown
                                            } else {
                                                gpui_kit::component::IconName::ChevronRight
                                            })
                                            .tooltip(if expanded {
                                                "Collapse container"
                                            } else {
                                                "Expand container"
                                            })
                                            .on_click(cx.listener(move |view, _, _, cx| {
                                                if !view.expanded_sections.remove(&key) {
                                                    view.expanded_sections.insert(key.clone());
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(SelectableText::new(
                                                "name",
                                                format!("{}{}", container.category, container.name),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_md()
                                            .text_xs()
                                            .bg(cx.theme().tone_surface(tone))
                                            .text_color(cx.theme().tone(tone))
                                            .child(SelectableText::new("state", container.state)),
                                    )
                                    .when(container.restarts > 0, |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(SelectableText::new(
                                                    "restarts",
                                                    format!("{} restarts", container.restarts),
                                                )),
                                        )
                                    }),
                            )
                            .when(expanded, |this| this.child(body))
                    }),
            )
    }

    /// One editor per key, and one Save for all of them.
    ///
    /// The point of the tab: a Secret read through YAML is base64, which is
    /// not something a person can edit, and a ConfigMap's values are folded
    /// into a YAML block scalar where indentation is part of the syntax. Here
    /// each value is just its own text.
    fn render_data(&self, cx: &mut Context<Self>) -> AnyElement {
        let Data::Ready(keys) = &self.data else {
            return self.notice("Reading the data…", Tone::Progressing, cx);
        };
        if keys.is_empty() {
            return self.notice("This one has no data.", Tone::Unknown, cx);
        }

        let may_apply = crate::actions::may_apply(&self.kind, self.rules.as_deref());
        let secret = self.kind.resource.kind == "Secret";
        let covered = secret && !self.revealed;

        let rows = keys.iter().map(|key| {
            let bytes = format!("{} bytes", key.entry.bytes);
            let field = if key.entry.field == beacon_kube::DataField::BinaryData {
                "binaryData"
            } else {
                ""
            };

            v_flex()
                .w_full()
                .gap_1()
                .child(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .gap_2()
                        .items_baseline()
                        .child(
                            div()
                                .font_family("monospace")
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(key.entry.key.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(bytes),
                        )
                        .when(!field.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(field),
                            )
                        }),
                )
                .child(match (&key.editor, covered) {
                    // Hidden rather than masked: a masked box still invites
                    // typing into something you cannot read.
                    (Some(_), true) => div()
                        .w_full()
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .bg(cx.theme().muted.opacity(0.5))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Hidden. Reveal to read or edit it.")
                        .into_any_element(),
                    (Some(editor), false) => Textarea::new(editor)
                        .readonly(!may_apply || self.busy())
                        .into_any_element(),
                    (None, _) => div()
                        .w_full()
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .bg(cx.theme().muted.opacity(0.5))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Not text. Editing it here would corrupt it.")
                        .into_any_element(),
                })
        });

        v_flex()
            .size_full()
            .child(
                div()
                    .id("data-keys")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .child(v_flex().w_full().gap_4().children(rows)),
            )
            .child(self.render_data_bar(secret, may_apply, cx))
            .into_any_element()
    }

    /// The bar under the keys: the same status line the YAML tab has, a
    /// reveal for Secrets, and Save.
    fn render_data_bar(
        &self,
        secret: bool,
        may_apply: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let running = self.busy();
        let revealed = self.revealed;

        v_flex()
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_2()
            .items_start()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(div().w_full().child(self.render_apply_status(cx)))
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .when(matches!(self.apply, Apply::Refused(_)), |this| {
                        this.child(
                            Button::new("force-apply-data")
                                .danger()
                                .small()
                                .label("Save anyway")
                                .disabled(running || !may_apply)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.apply_data(true, window, cx)
                                })),
                        )
                    })
                    .when(secret, |this| {
                        this.child(
                            Button::new("reveal")
                                .ghost()
                                .small()
                                .label(if revealed { "Hide" } else { "Reveal" })
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.revealed = !view.revealed;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("save-data")
                            .primary()
                            .small()
                            .label(if self.reviewing {
                                "Reviewing…"
                            } else if running {
                                "Saving…"
                            } else {
                                "Save"
                            })
                            .disabled(!may_apply || running || (secret && !revealed))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.apply_data(false, window, cx)
                            })),
                    ),
            )
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
                                .readonly(!may_apply || self.busy())
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
        let running = self.busy();

        v_flex()
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_2()
            .items_start()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(div().w_full().child(self.render_apply_status(cx)))
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .when(matches!(self.apply, Apply::Refused(_)), |this| {
                        this.child(
                            Button::new("force-apply")
                                .danger()
                                .small()
                                .label("Apply anyway")
                                .disabled(running || !may_apply)
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.apply(true, window, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new("format-yaml")
                            .ghost()
                            .small()
                            .label("Format")
                            .tooltip("Rewrite it the way the pane does — comments are not kept")
                            .disabled(!may_apply || running)
                            .on_click(cx.listener(|view, _, window, cx| view.format(window, cx))),
                    )
                    .child(
                        Button::new("apply")
                            .primary()
                            .small()
                            .label(if self.reviewing {
                                "Reviewing…"
                            } else if running {
                                "Applying…"
                            } else {
                                "Apply"
                            })
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
        if self.reviewing {
            return div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Preparing YAML changes for review…")
                .into_any_element();
        }
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
                .child(copyable_text("apply-error", error.clone()))
                .into_any_element(),
            Apply::Refused(conflict) => {
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(cx.theme().tone(Tone::Warning))
                            .child(copyable_text(
                                "apply-conflict",
                                format!("Not applied — {}", conflict.summary()),
                            )),
                    )
                    .children(conflict.fields.iter().enumerate().map(|(index, field)| {
                        let mine = self.value_at(field, cx);
                        h_flex()
                            .id(("conflict-field", index))
                            .flex_wrap()
                            .gap_2()
                            .items_baseline()
                            .text_xs()
                            .child(
                                div()
                                    .font_family("monospace")
                                    .text_color(cx.theme().foreground)
                                    .child(copyable_text("field", field.clone())),
                            )
                            .child(div().text_color(cx.theme().muted_foreground).child(
                                copyable_text(
                                    "yours",
                                    match mine {
                                        Some(value) => format!("yours: {value}"),
                                        None => "yours: (removed)".to_string(),
                                    },
                                ),
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
                    .children(events.into_iter().enumerate().map(|(index, (_, event))| {
                        let tone = if event.is_warning() {
                            Tone::Warning
                        } else {
                            Tone::Unknown
                        };

                        v_flex()
                            .id(("event", index))
                            .w_full()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap_2()
                                    .items_baseline()
                                    .text_xs()
                                    .child(
                                        div()
                                            .text_color(cx.theme().tone(tone))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(copyable_text("event-reason", event.reason)),
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
                            .child(
                                div()
                                    .text_sm()
                                    .child(copyable_text("event-message", event.message)),
                            )
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
            .child(copyable_text("detail-notice", message))
            .into_any_element()
    }

    // MARK: reading the object

    fn render_owners(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let direct = owner_links(&self.object);
        let owners: Vec<_> = direct
            .into_iter()
            .flat_map(|owner| {
                self.resolved_owners
                    .get(&owner.uid)
                    .cloned()
                    .unwrap_or_else(|| vec![owner])
            })
            .collect();
        v_flex()
            .w_full()
            .gap_0p5()
            .py_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Owner"),
            )
            .when(owners.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(SelectableText::new("no-owner", "<none>")),
                )
            })
            .children(owners.iter().enumerate().map(|(index, owner)| {
                let kind = owner_kind(
                    self.session.discovery().kinds(),
                    &owner.api_version,
                    &owner.kind,
                )
                .cloned()
                .map(Arc::new);
                let label = format!("{}/{}", owner.kind, owner.name);
                let target = kind.as_ref().map(|kind| {
                    ObjectRef::new(
                        if kind.namespaced {
                            self.target.namespace.clone()
                        } else {
                            None
                        },
                        owner.name.clone(),
                    )
                });
                div()
                    .id(("owner", index))
                    .w_full()
                    .text_sm()
                    .whitespace_normal()
                    .child(match kind.zip(target) {
                        Some((kind, target)) => Link::new("open-owner")
                            .accessibility_label(label.clone())
                            .cursor_pointer()
                            .text_color(cx.theme().resource_link())
                            .hover(|this| this.underline())
                            .child(SelectableText::new("value", label))
                            .on_activate(cx.listener(move |_, event, window, cx| {
                                // A selection gesture still lets the owner value be copied.
                                if matches!(event, ClickEvent::Mouse(_))
                                    && !TextSelection::selected_text(window, cx).is_empty()
                                {
                                    return;
                                }
                                cx.emit(OwnerRequested {
                                    kind: kind.clone(),
                                    target: target.clone(),
                                });
                            }))
                            .into_any_element(),
                        None => SelectableText::new("value", label).into_any_element(),
                    })
            }))
    }

    /// The containers of a Pod, merged with what their statuses say.
    ///
    /// `None` for anything that is not a Pod, which is what keeps the section
    /// out of a Deployment's overview.
    fn containers(&self) -> Option<Vec<ContainerLine>> {
        let path =
            crate::overview::pod_spec_path(&self.kind.resource.group, &self.kind.resource.kind)?;
        let spec = self.object.data.pointer(path)?;
        let mut containers = Vec::new();
        for (spec_key, status_key, category) in [
            ("containers", "containerStatuses", ""),
            ("initContainers", "initContainerStatuses", "Init · "),
            (
                "ephemeralContainers",
                "ephemeralContainerStatuses",
                "Ephemeral · ",
            ),
        ] {
            for container in spec
                .get(spec_key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = container["name"].as_str().unwrap_or_default();
                let status =
                    if self.kind.resource.kind == "Pod" && self.kind.resource.group.is_empty() {
                        self.object.data["status"][status_key]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|status| status["name"].as_str() == Some(name))
                    } else {
                        None
                    };
                let mut fields = container.clone();
                if let Value::Object(fields) = &mut fields {
                    fields.remove("name");
                    fields.remove("image");
                }
                containers.push(ContainerLine {
                    name: name.to_string(),
                    category,
                    image: container["image"]
                        .as_str()
                        .unwrap_or("<no image>")
                        .to_string(),
                    ports: container
                        .get("ports")
                        .and_then(Value::as_array)
                        .filter(|v| !v.is_empty())
                        .map(|ports| {
                            ports
                                .iter()
                                .map(|port| {
                                    let name = port["name"]
                                        .as_str()
                                        .map(|v| format!("{v}: "))
                                        .unwrap_or_default();
                                    format!(
                                        "{name}{} / {}",
                                        port["containerPort"],
                                        port["protocol"].as_str().unwrap_or("TCP")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        }),
                    resources: crate::overview::resource_table(
                        &container["resources"]["requests"],
                        &container["resources"]["limits"],
                        ["Resource", "Requests", "Limits"],
                    ),
                    ready: status.and_then(|s| s["ready"].as_bool()).unwrap_or(false),
                    restarts: status.and_then(|s| s["restartCount"].as_i64()).unwrap_or(0),
                    state: status.map(container_state).unwrap_or_else(|| {
                        if self.kind.resource.kind == "Pod" {
                            "Pending"
                        } else {
                            "Template"
                        }
                        .into()
                    }),
                    fields: {
                        let mut rows = crate::overview::rows(&fields);
                        rows.sort_by_key(|(key, _)| match key.split(" / ").next() {
                            Some("ports") => 0,
                            Some("resources") => 1,
                            Some("readinessProbe" | "livenessProbe" | "startupProbe") => 2,
                            _ => 3,
                        });
                        rows
                    },
                    status_fields: status.map(crate::overview::rows).unwrap_or_default(),
                });
            }
        }
        Some(containers)
    }

    /// Start a foreground-safe one-shot read whenever the Pod's owner identities change.
    fn resolve_pod_owners(&mut self, cx: &mut Context<Self>) {
        if !self.kind.resource.group.is_empty() || self.kind.resource.kind != "Pod" {
            return;
        }
        let owners = owner_links(&self.object);
        if owners == self.owner_sources {
            return;
        }
        self.owner_sources = owners.clone();
        self.resolved_owners.clear();
        self._owners_task = None;
        let targets: Vec<_> = owners
            .into_iter()
            .filter(|owner| owner.kind == "ReplicaSet" && owner.api_version.starts_with("apps/"))
            .filter_map(|owner| {
                owner_kind(
                    self.session.discovery().kinds(),
                    &owner.api_version,
                    &owner.kind,
                )
                .cloned()
                .map(|kind| (owner, kind.resource))
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let session = self.session.clone();
        let namespace = self.target.namespace.clone();
        let fetching = Bridge::global(cx).run(async move {
            let mut resolved = BTreeMap::new();
            for (owner, resource) in targets {
                if let Ok(replica_set) = session
                    .clone()
                    .get_object(resource, namespace.clone(), owner.name.clone())
                    .await
                {
                    // A replacement with the same name is not this Pod's owner.
                    let parents = replica_set_parents(&owner, &replica_set);
                    if !parents.is_empty() {
                        resolved.insert(owner.uid, parents);
                    }
                }
            }
            resolved
        });
        let sources = self.owner_sources.clone();
        self._owners_task = Some(cx.spawn(async move |this, cx| {
            if let Ok(resolved) = fetching.await {
                let _ = this.update(cx, |view, cx| {
                    if view.owner_sources == sources {
                        view.resolved_owners = resolved;
                        cx.notify();
                    }
                });
            }
        }));
    }
}

struct ContainerLine {
    name: String,
    category: &'static str,
    fields: Vec<(String, Option<String>)>,
    status_fields: Vec<(String, Option<String>)>,
    image: String,
    ports: Option<String>,
    resources: crate::overview::Table,
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

impl Render for DetailView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            DetailTab::Overview => self.render_overview(cx),
            DetailTab::Pods => self.render_node_pods(cx),
            DetailTab::Data => self.render_data(cx),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnerLink {
    api_version: String,
    kind: String,
    name: String,
    uid: String,
}

fn replica_set_parents(owner: &OwnerLink, replica_set: &DynamicObject) -> Vec<OwnerLink> {
    if replica_set.metadata.uid.as_deref() != Some(owner.uid.as_str()) {
        return Vec::new();
    }
    owner_links(replica_set)
}

fn owner_links(object: &DynamicObject) -> Vec<OwnerLink> {
    object
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|owner| OwnerLink {
            api_version: owner.api_version.clone(),
            kind: owner.kind.clone(),
            name: owner.name.clone(),
            uid: owner.uid.clone(),
        })
        .collect()
}

/// Owner references may use an older API version; navigation uses the served
/// preferred version but must still match the API group, including for CRDs.
fn owner_kind<'a>(kinds: &'a [Kind], api_version: &str, name: &str) -> Option<&'a Kind> {
    let group = api_version.rsplit_once('/').map_or("", |(group, _)| group);
    kinds
        .iter()
        .find(|kind| kind.resource.group == group && kind.resource.kind == name)
}

fn tls_certificates(
    kind: &Kind,
    object: &DynamicObject,
) -> Option<Result<Vec<CertificateInfo>, String>> {
    (kind.resource.group.is_empty()
        && kind.resource.kind == "Secret"
        && object.data.get("type").and_then(Value::as_str) == Some("kubernetes.io/tls"))
    .then(|| tls::inspect(object))
}

/// Reads a YAML document into the value the rest of this works with.
fn parse(yaml: &str) -> Result<Value, String> {
    serde_saphyr::from_str(yaml).map_err(|error| format!("This is not valid YAML: {error}"))
}

/// Rewrites a YAML document by round-tripping it through that value.
///
/// What this changes is shape, not content: flow style becomes block style,
/// indentation and quoting become the serialiser's. **Key order is left as
/// written** -- something in the dependency tree turns on
/// `serde_json/preserve_order`, so the map is an `IndexMap` and formatting
/// does not shuffle a document into alphabetical order behind the user's back.
///
/// Comments and blank lines do not survive, because there is nowhere in a
/// `serde_json::Value` for them to live. That is worth being plain about, but
/// it is not a new loss: applying already sends the parsed object, so anything
/// the parse drops was never going to reach the cluster either.
fn reformat(yaml: &str) -> Result<String, String> {
    serde_saphyr::to_string(&parse(yaml)?)
        .map_err(|error| format!("Could not write the YAML back out: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{container_state, owner_kind, owner_links, reformat, replica_set_parents};
    use beacon_kube::{ApiResource, GroupVersionKind, Kind};
    use serde_json::json;

    fn served_kind(group: &str, name: &str) -> Kind {
        Kind {
            resource: ApiResource::from_gvk_with_plural(
                &GroupVersionKind::gvk(group, "v1", name),
                &format!("{}s", name.to_lowercase()),
            ),
            namespaced: true,
            verbs: vec!["list".into(), "watch".into()],
        }
    }

    #[test]
    fn default_yaml_folds_only_match_the_requested_resource_paths() {
        let yaml = reformat("metadata:\n  name: web\n  annotations:\n    status:\n      text: value\n  managedFields:\n    - manager: beacon\n      operation: Apply\nspec:\n  status:\n    nested: value\n  managedFields:\n    - nested: value\nstatus:\n  phase: Running\n  ready: true\n").unwrap();
        let headers: Vec<_> = crate::yaml_folding::YamlFolding::default()
            .initial_lines(&yaml)
            .into_iter()
            .map(|line| yaml.lines().nth(line).unwrap())
            .collect();
        assert_eq!(headers, ["  managedFields:", "status:"]);
    }

    #[test]
    fn missing_or_inline_yaml_fields_do_not_request_folding() {
        for yaml in [
            "kind: ConfigMap\nmetadata:\n  name: web\n",
            "metadata:\n  managedFields: []\nstatus: {}\n",
            "data:\n  script: |\n    metadata:\n      managedFields:\n        - literal\n    status:\n      literal: value\n",
        ] {
            assert!(
                crate::yaml_folding::YamlFolding::default()
                    .initial_lines(yaml)
                    .is_empty(),
                "{yaml}"
            );
        }
    }

    #[test]
    fn resolving_a_replica_set_requires_the_same_uid_and_preserves_fallback() {
        let pod = serde_json::from_value(json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"pod","ownerReferences":[{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"rs","uid":"rs-uid"}]}})).unwrap();
        let owner = owner_links(&pod).remove(0);
        let mut replica_set = serde_json::from_value(json!({"apiVersion":"apps/v1","kind":"ReplicaSet","metadata":{"name":"rs","uid":"rs-uid","ownerReferences":[{"apiVersion":"apps/v1","kind":"Deployment","name":"app","uid":"app-uid"}]}})).unwrap();
        assert_eq!(
            replica_set_parents(&owner, &replica_set)[0].kind,
            "Deployment"
        );
        replica_set.metadata.uid = Some("replacement".into());
        assert!(replica_set_parents(&owner, &replica_set).is_empty());
        replica_set.metadata.uid = Some("rs-uid".into());
        replica_set.metadata.owner_references = None;
        assert!(replica_set_parents(&owner, &replica_set).is_empty());
    }

    #[test]
    fn owner_navigation_matches_group_and_uses_served_version() {
        let kinds = [
            served_kind("example.io", "ReplicaSet"),
            served_kind("apps", "ReplicaSet"),
        ];
        let owner = owner_kind(&kinds, "apps/v1beta2", "ReplicaSet").unwrap();
        assert_eq!(owner.resource.group, "apps");
        assert_eq!(owner.resource.version, "v1");
        assert!(owner_kind(&kinds, "unknown.io/v1", "ReplicaSet").is_none());
    }

    #[test]
    fn core_owner_does_not_resolve_to_a_same_named_custom_kind() {
        let kinds = [served_kind("example.io", "Node"), served_kind("", "Node")];
        assert_eq!(owner_kind(&kinds, "v1", "Node").unwrap().resource.group, "");
    }

    /// Flow style is expanded, and the keys stay in the order they were
    /// written -- a formatter that silently alphabetised somebody's manifest
    /// would be worse than no formatter.
    #[test]
    fn formatting_expands_flow_style_and_keeps_key_order() {
        let messy = "kind: Pod\napiVersion: v1\nmetadata: {name: web, labels: {app: web}}\n";
        assert_eq!(
            reformat(messy).unwrap(),
            "kind: Pod\napiVersion: v1\nmetadata:\n  name: web\n  labels:\n    app: web\n"
        );
    }

    #[test]
    fn formatting_twice_changes_nothing_the_second_time() {
        let once =
            reformat("kind: Pod\napiVersion: v1\nspec: {containers: [{name: web}]}\n").unwrap();
        assert_eq!(reformat(&once).unwrap(), once);
    }

    /// Pinned rather than discovered later: the tooltip says so, and a test is
    /// what stops it quietly becoming untrue.
    #[test]
    fn formatting_drops_comments() {
        assert_eq!(reformat("# the app\nkind: Pod\n").unwrap(), "kind: Pod\n");
    }

    #[test]
    fn invalid_yaml_is_named_as_such() {
        let error = reformat("kind: Pod\n  bad indent: yes\n").unwrap_err();
        assert!(error.starts_with("This is not valid YAML:"), "{error}");
    }

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
}
