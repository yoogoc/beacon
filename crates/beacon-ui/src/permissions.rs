//! Server-backed Can I and an explainable binding → role → rule view.
use crate::{
    bridge::Bridge, copyable_text::copyable_text, detail::OwnerRequested, theme::BeaconTheme as _,
};
use beacon_kube::{
    ClusterSession, Kind, ObjectRef,
    rbac::{self, Grant, Identity, Query, Subject},
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, TitleBar, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;
type Picker = Entity<SelectState<SearchableVec<String>>>;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Current,
    ServiceAccount,
    User,
    Group,
}
fn input(value: &str, placeholder: &str, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| {
        let mut input = InputState::new(window, cx).placeholder(placeholder.to_owned());
        input.set_value(value.to_owned(), window, cx);
        input
    })
}
#[derive(Clone)]
struct Resource {
    label: String,
    group: String,
    plural: String,
    value: String,
}
impl SelectItem for Resource {
    type Value = String;
    fn title(&self) -> SharedString {
        self.label.clone().into()
    }
    fn value(&self) -> &String {
        &self.value
    }
    fn matches(&self, query: &str) -> bool {
        self.label.to_lowercase().contains(&query.to_lowercase())
            || self.plural.to_lowercase().contains(&query.to_lowercase())
    }
}
pub(crate) struct Permissions {
    session: Arc<ClusterSession>,
    mode: Mode,
    name: Entity<InputState>,
    sa_namespace: Entity<InputState>,
    groups: Entity<InputState>,
    namespace: Entity<InputState>,
    subresource: Entity<InputState>,
    object_name: Entity<InputState>,
    search: Entity<InputState>,
    verb: Picker,
    resource: Entity<SelectState<SearchableVec<Resource>>>,
    resources: Vec<Resource>,
    identity: Option<Identity>,
    grants: Vec<Grant>,
    visible: Vec<usize>,
    list: ListState,
    errors: Vec<String>,
    answer: Option<(bool, String)>,
    loading: bool,
    checking: bool,
    dirty: bool,
    _load: Option<Task<()>>,
    _query: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<OwnerRequested> for Permissions {}
impl Permissions {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        subject: Subject,
        query: Query,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (mode, name, sa_namespace, groups) = match subject {
            Subject::Current => (
                Mode::Current,
                String::new(),
                "default".into(),
                String::new(),
            ),
            Subject::ServiceAccount { namespace, name } => {
                (Mode::ServiceAccount, name, namespace, String::new())
            }
            Subject::User { name, groups } => {
                (Mode::User, name, "default".into(), groups.join(", "))
            }
            Subject::Group(name) => (Mode::Group, name, "default".into(), String::new()),
        };
        let name = input(&name, "Subject name", window, cx);
        let sa_namespace = input(&sa_namespace, "ServiceAccount namespace", window, cx);
        let groups = input(&groups, "Groups, separated by commas", window, cx);
        let namespace = input(
            query.namespace.as_deref().unwrap_or(""),
            "Namespace (empty = all namespaces / cluster scope)",
            window,
            cx,
        );
        let subresource = input(
            &query.subresource,
            "Subresource, e.g. log or exec",
            window,
            cx,
        );
        let object_name = input(
            query.name.as_deref().unwrap_or(""),
            "Resource name (optional)",
            window,
            cx,
        );
        let search = input("", "Filter rules by verb, resource or binding", window, cx);
        let verb = cx.new(|cx| {
            let mut state = SelectState::new(
                SearchableVec::new(
                    [
                        "get",
                        "list",
                        "watch",
                        "create",
                        "update",
                        "patch",
                        "delete",
                        "deletecollection",
                        "bind",
                        "escalate",
                        "impersonate",
                    ]
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
                ),
                None,
                window,
                cx,
            )
            .searchable(true);
            state.set_selected_value(
                &if query.verb.is_empty() {
                    "get".into()
                } else {
                    query.verb.clone()
                },
                window,
                cx,
            );
            state
        });
        let mut resources: Vec<_> = session
            .discovery()
            .kinds()
            .iter()
            .map(|kind| Resource {
                label: kind.display_name(),
                group: kind.resource.group.clone(),
                plural: kind.resource.plural.clone(),
                value: format!("{}/{}", kind.resource.group, kind.resource.plural),
            })
            .collect();
        let selected = format!("{}/{}", query.group, query.resource);
        if !query.resource.is_empty()
            && !resources.iter().any(|resource| resource.value == selected)
        {
            resources.push(Resource {
                label: format!("{} · {}", query.resource, query.group),
                group: query.group,
                plural: query.resource,
                value: selected.clone(),
            });
        }
        resources.sort_by(|a, b| a.label.cmp(&b.label));
        let resource = cx.new(|cx| {
            let mut state =
                SelectState::new(SearchableVec::new(resources.clone()), None, window, cx)
                    .searchable(true);
            if resources.iter().any(|r| r.value == selected) {
                state.set_selected_value(&selected, window, cx);
            }
            state
        });
        let mut subscriptions = Vec::new();
        for field in [&name, &sa_namespace, &groups, &namespace] {
            subscriptions.push(cx.subscribe(field, |view, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view.invalidate(cx);
                }
            }));
        }
        for field in [&subresource, &object_name] {
            subscriptions.push(cx.subscribe(field, |view, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view._query = None;
                    view.checking = false;
                    view.answer = None;
                    cx.notify();
                }
            }));
        }
        subscriptions.push(cx.subscribe(&search, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.filter(cx);
            }
        }));
        subscriptions.push(cx.subscribe(
            &verb,
            |view, _, _: &SelectEvent<SearchableVec<String>>, cx| {
                view.answer = None;
                view._query = None;
                view.checking = false;
                cx.notify();
            },
        ));
        subscriptions.push(cx.subscribe(
            &resource,
            |view, _, _: &SelectEvent<SearchableVec<Resource>>, cx| {
                view.answer = None;
                view._query = None;
                view.checking = false;
                cx.notify();
            },
        ));
        let mut view = Self {
            session,
            mode,
            name,
            sa_namespace,
            groups,
            namespace,
            subresource,
            object_name,
            search,
            verb,
            resource,
            resources,
            identity: None,
            grants: vec![],
            visible: vec![],
            list: ListState::new(0, ListAlignment::Top, px(140.)),
            errors: vec![],
            answer: None,
            loading: false,
            checking: false,
            dirty: true,
            _load: None,
            _query: None,
            _subscriptions: subscriptions,
        };
        view.inspect(window, cx);
        view
    }
    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self._load = None;
        self._query = None;
        self.loading = false;
        self.checking = false;
        self.identity = None;
        self.grants.clear();
        self.visible.clear();
        self.list.reset(0);
        self.answer = None;
        self.errors.clear();
        self.dirty = true;
        cx.notify();
    }
    fn subject(&self, cx: &App) -> Result<Subject, String> {
        let name = self.name.read(cx).value().trim().to_string();
        if self.mode != Mode::Current && name.is_empty() {
            return Err("Enter the subject name.".into());
        }
        let ns = self.sa_namespace.read(cx).value().trim().to_string();
        Ok(match self.mode {
            Mode::Current => Subject::Current,
            Mode::ServiceAccount => {
                if ns.is_empty() {
                    return Err("Enter the ServiceAccount namespace.".into());
                }
                Subject::ServiceAccount {
                    namespace: ns,
                    name,
                }
            }
            Mode::User => Subject::User {
                name,
                groups: self
                    .groups
                    .read(cx)
                    .value()
                    .split(',')
                    .map(str::trim)
                    .filter(|group| !group.is_empty())
                    .map(str::to_owned)
                    .collect(),
            },
            Mode::Group => Subject::Group(name),
        })
    }
    fn query(&self, cx: &App) -> Result<Query, String> {
        let selected = self
            .resource
            .read(cx)
            .selected_value()
            .ok_or("Choose a resource.")?;
        let resource = self
            .resources
            .iter()
            .find(|resource| &resource.value == selected)
            .ok_or("Resource is no longer available.")?;
        let optional = |field: &Entity<InputState>| {
            let value = field.read(cx).value().trim().to_string();
            (!value.is_empty()).then_some(value)
        };
        Ok(Query {
            verb: self
                .verb
                .read(cx)
                .selected_value()
                .cloned()
                .ok_or("Choose a verb.")?,
            group: resource.group.clone(),
            resource: resource.plural.clone(),
            subresource: self.subresource.read(cx).value().trim().into(),
            namespace: optional(&self.namespace),
            name: optional(&self.object_name),
        })
    }
    fn inspect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let subject = match self.subject(cx) {
            Ok(subject) => subject,
            Err(error) => {
                self.errors = vec![error];
                cx.notify();
                return;
            }
        };
        let namespace = self.namespace.read(cx).value().trim().to_string();
        let namespace = (!namespace.is_empty()).then_some(namespace);
        self._load = None;
        self.loading = true;
        self.errors.clear();
        self.grants.clear();
        self.visible.clear();
        self.list.reset(0);
        self.identity = None;
        self.dirty = false;
        let session = self.session.clone();
        let task = Bridge::global(cx).run_cancellable(async move {
            let identity = match subject.identity() {
                Some(identity) => Ok(identity),
                None => rbac::current_identity(&session)
                    .await
                    .map_err(|error| error.user_message()),
            };
            let catalog = rbac::catalog(session, namespace).await;
            let grants = identity
                .as_ref()
                .map(|identity| rbac::grants(identity, &catalog.objects))
                .unwrap_or_default();
            (identity, catalog.errors, grants)
        });
        self._load=Some(cx.spawn_in(window,async move|view,cx| {let result=task.result().await;let _=view.update(cx,|view,cx|{view.loading=false;match result {Ok((identity,errors,grants))=>{view.errors=errors;match identity {Ok(identity)=>view.identity=Some(identity),Err(error)=>view.errors.push(format!("Identity could not be read: {error}. Can I can still query the current user."))}view.grants=grants;},Err(error)=>view.errors.push(error.to_string())}view.filter(cx);cx.notify();});}));
        cx.notify();
    }
    fn check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (subject, query) = match self
            .subject(cx)
            .and_then(|subject| self.query(cx).map(|query| (subject, query)))
        {
            Ok(values) => values,
            Err(error) => {
                self.answer = Some((false, error));
                cx.notify();
                return;
            }
        };
        self._query = None;
        self.checking = true;
        self.answer = None;
        let session = self.session.clone();
        let description = query.describe();
        let task = Bridge::global(cx).run_cancellable(async move {
            rbac::can_i(&session, &subject, &query)
                .await
                .map_err(|error| error.user_message())
        });
        self._query = Some(cx.spawn_in(window, async move |view, cx| {
            let result = task.result().await;
            let _ = view.update(cx, |view, cx| {
                view.checking = false;
                view.answer = Some(match result {
                    Ok(Ok(answer)) => {
                        let mut text = format!(
                            "{}: {}",
                            if answer.allowed {
                                "Allowed"
                            } else {
                                "Not allowed"
                            },
                            description
                        );
                        if let Some(reason) = answer.reason.filter(|s| !s.is_empty()) {
                            text.push_str(&format!("\n{reason}"));
                        }
                        if let Some(error) = answer.evaluation_error.filter(|s| !s.is_empty()) {
                            text.push_str(&format!("\nEvaluation incomplete: {error}"));
                        }
                        (answer.allowed, text)
                    }
                    Ok(Err(error)) => {
                        (false, format!("Permission could not be evaluated: {error}"))
                    }
                    Err(error) => (false, error.to_string()),
                });
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_lowercase();
        self.visible = self
            .grants
            .iter()
            .enumerate()
            .filter(|(_, grant)| {
                format!(
                    "{} {} {} {} {}",
                    grant.binding_kind,
                    grant.binding,
                    grant.role_kind,
                    grant.role,
                    serde_json::to_string(&grant.rules).unwrap_or_default()
                )
                .to_lowercase()
                .contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        self.list.reset(self.visible.len());
        cx.notify();
    }
    fn link(
        &self,
        id: impl Into<ElementId>,
        kind: &str,
        target: &ObjectRef,
        disabled: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let known = self
            .session
            .discovery()
            .kinds()
            .iter()
            .find(|candidate| {
                candidate.resource.group == rbac::GROUP && candidate.resource.kind == kind
            })
            .cloned()
            .map(Arc::new);
        let target = target.clone();
        Button::new(id)
            .ghost()
            .small()
            .label(format!("{kind} · {}", target.name))
            .text_color(cx.theme().resource_link())
            .disabled(disabled || known.is_none())
            .on_click(cx.listener(move |_, _, _, cx| {
                if let Some(kind) = known.clone() {
                    cx.emit(OwnerRequested {
                        kind,
                        target: target.clone(),
                    });
                }
            }))
            .into_any_element()
    }
    fn render_grant(&self, row: usize, cx: &Context<Self>) -> AnyElement {
        let grant = &self.grants[self.visible[row]];
        v_flex()
            .id(("permission-grant", row))
            .test_support()
            .p_3()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(self.link(
                        ("permission-binding", row),
                        &grant.binding_kind,
                        &grant.binding,
                        false,
                        cx,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("→"),
                    )
                    .child(self.link(
                        ("permission-role", row),
                        &grant.role_kind,
                        &grant.role,
                        grant.missing,
                        cx,
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(copyable_text(
                        ("permission-match", row),
                        format!(
                            "{} · applies in {}",
                            grant.matched_subjects.join(", "),
                            grant
                                .binding
                                .namespace
                                .as_deref()
                                .unwrap_or("all namespaces and cluster scope")
                        ),
                    )),
            )
            .when(grant.missing, |view| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(crate::i18n::text(
                            "Role is missing or could not be read. No rules are inferred.",
                        )),
                )
            })
            .children(grant.rules.iter().enumerate().map(|(index, rule)| {
                let fields = [
                    ("verbs", "Verbs"),
                    ("apiGroups", "API groups"),
                    ("resources", "Resources"),
                    ("resourceNames", "Names"),
                    ("nonResourceURLs", "URLs"),
                ]
                .into_iter()
                .filter_map(|(key, label)| {
                    rule.get(key).map(|value| {
                        format!(
                            "{label}: {}",
                            value
                                .as_array()
                                .map(|values| values
                                    .iter()
                                    .map(|value| if value == "" {
                                        "core".into()
                                    } else {
                                        value.as_str().unwrap_or("?").to_string()
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", "))
                                .unwrap_or_else(|| value.to_string())
                        )
                    })
                })
                .collect::<Vec<_>>()
                .join("\n");
                div()
                    .pl_2()
                    .border_l_2()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .child(copyable_text(
                        (SharedString::from(format!("permission-rule-{row}")), index),
                        fields,
                    ))
            }))
            .into_any_element()
    }
}
impl Render for Permissions {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let modes = [
            (Mode::Current, "Current user"),
            (Mode::ServiceAccount, "ServiceAccount"),
            (Mode::User, "User"),
            (Mode::Group, "Group"),
        ];
        v_flex().size_full().min_size_0().bg(cx.theme().background).text_color(cx.theme().foreground).on_action(|_:&crate::app::CloseTab,window,_|window.remove_window())
            .child(TitleBar::new().child(crate::i18n::text("Permissions")))
            .child(v_flex().p_4().gap_2().border_b_1().border_color(cx.theme().border)
                .child(h_flex().gap_1().children(modes.into_iter().enumerate().map(|(index,(mode,label))|Button::new(("permission-subject-mode",index)).ghost().small().label(label).selected(self.mode==mode).on_click(cx.listener(move|view,_,_,cx|{view.mode=mode;view.invalidate(cx);})))) )
                .when(self.mode!=Mode::Current,|view|view.child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.name).small())).when(self.mode==Mode::ServiceAccount,|row|row.child(div().flex_1().child(Input::new(&self.sa_namespace).small())))))
                .when(self.mode==Mode::User,|view|view.child(Input::new(&self.groups).small()))
                .child(h_flex().gap_2().items_center().child(div().flex_1().child(Input::new(&self.namespace).small())).child(Button::new("permission-inspect").small().outline().label(crate::i18n::text(if self.loading{"Loading…"}else{"Inspect bindings"})).disabled(self.loading).on_click(cx.listener(|view,_,window,cx|view.inspect(window,cx)))))
                .child(h_flex().gap_2().child(div().w(px(140.)).child(Select::new(&self.verb).small())).child(div().flex_1().min_w_0().child(Select::new(&self.resource).small())))
                .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.subresource).small())).child(div().flex_1().child(Input::new(&self.object_name).small())).child(Button::new("permission-can-i").small().primary().label(if self.checking{"Checking…"}else{"Can I?"}).disabled(self.checking).on_click(cx.listener(|view,_,window,cx|view.check(window,cx)))))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text("Can I uses the API server. Other subjects require SubjectAccessReview permission. User groups must be entered explicitly; Group queries test that group's grants."))))
            .when_some(self.answer.as_ref(),|view,(allowed,message)|view.child(div().id("permission-answer").test_support().px_4().py_2().text_sm().text_color(if *allowed{cx.theme().success}else{cx.theme().warning}).child(copyable_text("permission-answer-text",message.clone()))))
            .when(!self.errors.is_empty(),|view|view.child(v_flex().id("permission-errors").max_h(px(140.)).overflow_y_scroll().children(self.errors.iter().enumerate().map(|(index,error)|div().id(("permission-error",index)).test_support().px_4().py_1().text_xs().text_color(cx.theme().warning).child(copyable_text(("permission-error-text",index),error.clone()))))))
            .child(v_flex().px_4().py_2().gap_1().child(div().text_xs().text_color(cx.theme().muted_foreground).child(if self.dirty{"Inspect bindings to refresh this subject and scope.".into()}else{format!("{} matching bindings · RBAC sources only; other authorizers may affect Can I",self.visible.len())})).child(Input::new(&self.search).small()))
            .child(div().flex_1().min_size_0().when(self.visible.is_empty(),|view|view.child(div().id("permission-empty").test_support().p_4().text_sm().text_color(cx.theme().muted_foreground).child(if self.loading{"Loading RBAC sources…"}else if self.identity.is_none()||!self.errors.is_empty(){"Bindings could not be fully checked. Review the messages above."}else{"No matching bindings in this scope."})))
                .when(!self.visible.is_empty(),|view|view.child(list(self.list.clone(),cx.processor(|view,row,_,cx|view.render_grant(row,cx))).size_full())))
    }
}
pub(crate) fn open(session: Arc<ClusterSession>, subject: Subject, query: Query, cx: &mut App) {
    cx.defer(move |cx| {
        let navigate = session.id().clone();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(980.), px(780.)),
                cx,
            ))),
            window_min_size: Some(size(px(680.), px(580.))),
            ..TitleBar::window_options()
        };
        if let Err(error) = cx.open_window(options, |window, cx| {
            crate::i18n::window_title("Beacon — Permissions", window, cx);
            let view = cx.new(|cx| Permissions::new(session, subject, query, window, cx));
            cx.subscribe(&view, move |_, event: &OwnerRequested, cx| {
                crate::app::navigate_resource(
                    &navigate,
                    event.kind.clone(),
                    event.target.clone(),
                    cx,
                )
            })
            .detach();
            window.activate_window();
            cx.new(|cx| gpui_kit::component::Root::new(view, window, cx))
        }) {
            tracing::error!(%error,"could not open permissions");
        }
    });
}
pub(crate) fn for_resource(
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    target: ObjectRef,
    error: Option<&str>,
    verb: &str,
    cx: &mut App,
) {
    let subject = if kind.resource.group.is_empty() && kind.resource.kind == "ServiceAccount" {
        Subject::ServiceAccount {
            namespace: target.namespace.clone().unwrap_or_else(|| "default".into()),
            name: target.name.clone(),
        }
    } else {
        Subject::Current
    };
    let query = error.and_then(rbac::denial).unwrap_or(Query {
        verb: verb.into(),
        group: kind.resource.group.clone(),
        resource: kind.resource.plural.clone(),
        namespace: target.namespace.clone(),
        name: Some(target.name),
        ..Default::default()
    });
    open(session, subject, query, cx);
}

pub(crate) fn error_link(
    id: impl Into<ElementId>,
    session: Arc<ClusterSession>,
    error: &str,
    fallback: Query,
) -> Option<AnyElement> {
    if !error.contains("403") && !error.to_lowercase().contains("forbidden") {
        return None;
    }
    let query = rbac::denial(error).unwrap_or(fallback);
    Some(
        Button::new(id)
            .small()
            .ghost()
            .label(crate::i18n::text("View permissions"))
            .on_click(move |_, _, cx| open(session.clone(), Subject::Current, query.clone(), cx))
            .into_any_element(),
    )
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests;
