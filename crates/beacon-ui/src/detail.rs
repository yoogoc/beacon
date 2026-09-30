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

use beacon_columns::{EventSummary, Timestamp, format_age, format_duration};
use beacon_kube::{
    Applied, ClusterSession, Conflict, DataEntry, DynamicObject, Kind, ObjectRef, Operation,
    ResourceStore, Rules, WatchKey, data,
};
use gpui_kit::base::{Link, SelectableText, TextSelection};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Textarea, TextareaState};
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
    /// A ConfigMap's or Secret's keys, one editor each.
    Data,
    Yaml,
    Events,
}

impl DetailTab {
    /// Pod streams and terminals belong to the independent bottom panel.
    fn for_kind(kind: &Kind) -> Vec<Self> {
        let mut tabs = vec![Self::Overview];
        if data::is_keyed(&kind.resource.group, &kind.resource.kind) {
            tabs.push(Self::Data);
        }
        tabs.extend([Self::Yaml, Self::Events]);
        tabs
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Overview => "Overview",
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
    overview_sections: crate::overview::Sections,
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

    events: ResourceStore,
    events_listed: bool,

    /// What this user may do here. `None` until the answer arrives; see
    /// [`crate::actions`].
    rules: Option<Arc<Rules>>,

    now: Timestamp,

    _yaml_task: Option<Task<()>>,
    _apply_task: Option<Task<()>>,
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
        let overview_sections =
            crate::overview::sections(&kind.resource.group, &kind.resource.kind, &object.data);
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
            overview_sections,
            resolved_owners: BTreeMap::new(),
            owner_sources: Vec::new(),
            _owners_task: None,
            yaml: Yaml::Unopened,
            data: Data::Unopened,
            revealed: false,
            yaml_editor,
            apply: Apply::Idle,
            events: ResourceStore::new(),
            events_listed: false,
            rules,
            now: Timestamp::now(),
            _yaml_task: None,
            _apply_task: None,
            _events_task: None,
            _clock: Task::ready(()),
        };

        this.resolve_pod_owners(cx);
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
        self.overview_sections = crate::overview::sections(
            &self.kind.resource.group,
            &self.kind.resource.kind,
            &object.data,
        );
        self.object = object;
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
        self.tab = tab;
        match tab {
            DetailTab::Data if matches!(self.data, Data::Unopened) => self.load_data(window, cx),
            DetailTab::Yaml if matches!(self.yaml, Yaml::Unopened) => self.load_yaml(window, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Sends the edited YAML back with Server-Side Apply.
    ///
    /// `force` takes ownership of the fields another manager holds. It is only
    /// ever reached from the conflict view, after the refusal has been read.
    pub fn apply(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let yaml = self.yaml_editor.read(cx).value().to_string();

        let object = match parse(&yaml) {
            Ok(object) => object,
            Err(error) => {
                self.apply = Apply::Failed(error);
                cx.notify();
                return;
            }
        };

        self.send(object, force, window, cx);
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

        self.send(object, force, window, cx);
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

        let mut sections = v_flex().gap_4().p_3().w_full();

        if let Some(certificates) = &self.certificates {
            sections = sections.child(self.render_certificates(certificates, cx));
        }

        let created = metadata
            .creation_timestamp
            .as_ref()
            .map(|at| format!("{} ago ({})", format_age(at, self.now), at.0));

        sections = sections.child(
            self.section(
                "Metadata",
                vec![
                    ("Created", created),
                    ("UID", metadata.uid.clone()),
                    ("Generation", metadata.generation.map(|v| v.to_string())),
                    ("Resource version", metadata.resource_version.clone()),
                    (
                        "Deletion timestamp",
                        metadata
                            .deletion_timestamp
                            .as_ref()
                            .map(|v| v.0.to_string()),
                    ),
                    (
                        "Finalizers",
                        metadata
                            .finalizers
                            .as_ref()
                            .filter(|v| !v.is_empty())
                            .map(|v| v.join(", ")),
                    ),
                ],
                cx,
            )
            .child(self.render_owners(cx)),
        );

        if let Some(labels) = &metadata.labels
            && !labels.is_empty()
        {
            sections = sections.child(self.metadata_entries(MetadataGroup::Labels, labels, cx));
        }
        if let Some(annotations) = &metadata.annotations
            && !annotations.is_empty()
        {
            sections =
                sections.child(self.metadata_entries(MetadataGroup::Annotations, annotations, cx));
        }

        if let Some(containers) = self.containers() {
            sections = sections.child(self.container_section(containers, cx));
        }

        for (title, rows) in &self.overview_sections {
            sections = sections.child(self.structured_section(title, rows, cx));
        }

        div()
            .id("overview")
            .size_full()
            .overflow_y_scroll()
            .child(sections)
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
                        .child(error.clone()),
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
                    ("Key details", certificate.public_key_details.clone()),
                    ("Cert SHA-256", Some(certificate.sha256_fingerprint.clone())),
                ],
                cx,
            ))
            .child(
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(self.heading("Extensions", cx))
                    .children(certificate.extensions.iter().enumerate().map(
                        |(index, extension)| {
                            v_flex()
                                .id(("extension", index))
                                .w_full()
                                .gap_0p5()
                                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(
                                    if extension.critical {
                                        format!("{} · critical", extension.name)
                                    } else {
                                        extension.name.clone()
                                    },
                                ))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(SelectableText::new(
                                            "details",
                                            extension.details.clone(),
                                        )),
                                )
                        },
                    )),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(self.heading("Public key PEM", cx))
                    .child(
                        div()
                            .w_full()
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
            )
            .into_any_element()
    }

    /// A titled block of label/value rows. Absent values are shown rather than
    /// hidden: "this object has no owner" is information.
    fn section(
        &self,
        title: impl Into<SharedString>,
        rows: Vec<(impl Into<SharedString>, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let title: SharedString = title.into();
        v_flex()
            .id(title.clone())
            .gap_1()
            .w_full()
            .child(self.heading(title, cx))
            .children(rows.into_iter().enumerate().map(|(index, (label, value))| {
                let label: SharedString = label.into();
                v_flex()
                    .id(("overview-field", index))
                    .w_full()
                    .min_w_0()
                    .gap_0p5()
                    .py_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                    .child(match value {
                        Some(value) => div()
                            .w_full()
                            .text_sm()
                            .whitespace_normal()
                            .child(SelectableText::new("value", value)),
                        None => div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(SelectableText::new("value", "<none>")),
                    })
            }))
    }

    fn heading(&self, title: impl Into<SharedString>, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().muted_foreground)
            .child(title.into().to_uppercase())
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
            .child(self.heading(group.title(), cx))
            .children(entries.iter().take(visible_count).map(|(key, value)| {
                div()
                    .w_full()
                    .min_w_0()
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().muted.opacity(0.5))
                    .text_sm()
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
                    .id(SharedString::from(format!("container-{}", container.name)))
                    .w_full()
                    .gap_0p5()
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().muted.opacity(0.5))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .items_center()
                            .text_sm()
                            .child(div().font_weight(FontWeight::MEDIUM).child(
                                SelectableText::new(
                                    "name",
                                    format!("{}{}", container.category, container.name),
                                ),
                            ))
                            .child(
                                div()
                                    .text_xs()
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
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SelectableText::new("image", container.image)),
                    )
                    .child(self.structured_section_key(
                        "Configuration",
                        &format!("{}-{}-configuration", container.category, container.name),
                        &container.fields,
                        cx,
                    ))
                    .when(!container.status_fields.is_empty(), |card| {
                        card.child(self.structured_section_key(
                            "Runtime",
                            &format!("{}-{}-runtime", container.category, container.name),
                            &container.status_fields,
                            cx,
                        ))
                    })
            }))
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
                        .readonly(!may_apply)
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
        let running = matches!(self.apply, Apply::Running);
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
                            .label(if running { "Saving…" } else { "Save" })
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
                            .flex_wrap()
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
                                    .flex_wrap()
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

    fn structured_section(
        &self,
        title: &str,
        rows: &[(String, Option<String>)],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.structured_section_key(title, title, rows, cx)
    }

    fn structured_section_key(
        &self,
        title: &str,
        section_key: &str,
        rows: &[(String, Option<String>)],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if rows.is_empty() {
            return div().into_any_element();
        }
        let expanded = self.expanded_sections.contains(section_key);
        let shown = if expanded {
            rows.len()
        } else {
            rows.len().min(12)
        };
        let key = section_key.to_string();
        self.section(
            SharedString::from(title.to_string()),
            rows[..shown].to_vec(),
            cx,
        )
        .when(rows.len() > 12, |section| {
            section.child(
                Button::new("toggle-fields")
                    .ghost()
                    .small()
                    .label(if expanded {
                        "Show less".into()
                    } else {
                        format!("Show {} more fields", rows.len() - shown)
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if !view.expanded_sections.remove(&key) {
                            view.expanded_sections.insert(key.clone());
                        }
                        cx.notify();
                    })),
            )
        })
        .into_any_element()
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
