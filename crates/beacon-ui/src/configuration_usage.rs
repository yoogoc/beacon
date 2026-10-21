//! Live configuration usage and missing references; contents of Secrets never reach this view.
use crate::{
    bridge::{Bridge, drain_into},
    copyable_text::copyable_text,
    detail::OwnerRequested,
    theme::BeaconTheme as _,
};
use beacon_kube::{
    ClusterSession, Delta, DynamicObject, Kind, ObjectRef, ResourceStore, WatchKey,
    config_refs::{self, ConfigurationKind, Reference},
};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{collections::BTreeSet, sync::Arc};

pub(crate) fn supported(kind: &Kind) -> bool {
    ConfigurationKind::for_kind(&kind.resource.group, &kind.resource.kind).is_some()
        || config_refs::supported(&kind.resource.group, &kind.resource.kind)
}

struct Source {
    kind: Arc<Kind>,
    store: ResourceStore,
    listed: bool,
    error: Option<String>,
    _list: Option<Task<()>>,
    _watch: Option<Task<()>>,
}
#[derive(Clone)]
struct Row {
    kind: Option<Arc<Kind>>,
    target: ObjectRef,
    label: String,
    references: Vec<Reference>,
    missing: BTreeSet<String>,
    absent: bool,
    unchecked: bool,
}

pub(crate) struct ConfigurationUsageView {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    object: Arc<DynamicObject>,
    sources: Vec<Source>,
    search: Entity<InputState>,
    rows: Vec<Row>,
    list: ListState,
    preparing: bool,
    _search: Subscription,
    _rebuild: Option<Task<()>>,
}
impl EventEmitter<OwnerRequested> for ConfigurationUsageView {}

impl ConfigurationUsageView {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        object: Arc<DynamicObject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let by = ConfigurationKind::for_kind(&kind.resource.group, &kind.resource.kind).is_some();
        let sources = session
            .discovery()
            .kinds()
            .iter()
            .filter(|kind| {
                if by {
                    config_refs::supported(&kind.resource.group, &kind.resource.kind)
                } else {
                    ConfigurationKind::for_kind(&kind.resource.group, &kind.resource.kind).is_some()
                }
            })
            .map(|kind| Source {
                kind: Arc::new(kind.clone()),
                store: ResourceStore::new(),
                listed: false,
                error: None,
                _list: None,
                _watch: None,
            })
            .collect();
        let search = cx.new(|cx| {
            crate::i18n::input("Search resources, keys or paths", window, cx).clean_on_escape()
        });
        let subscription = cx.subscribe(&search, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.rebuild(cx);
            }
        });
        let mut view = Self {
            session,
            kind,
            object,
            sources,
            search,
            rows: vec![],
            list: ListState::new(0, ListAlignment::Top, px(150.)),
            preparing: false,
            _search: subscription,
            _rebuild: None,
        };
        view.load(window, cx);
        view
    }
    pub(crate) fn refresh(&mut self, object: Arc<DynamicObject>, cx: &mut Context<Self>) {
        self.object = object;
        self.rebuild(cx);
    }
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for index in 0..self.sources.len() {
            let source = &mut self.sources[index];
            source._watch = None;
            source._list = None;
            source.listed = false;
            source.error = None;
            source.store = ResourceStore::new();
            if !source.kind.supports("list") {
                source.listed = true;
                source.error = Some(
                    crate::i18n::message(
                        "messages.cannot_be_listed",
                        &[("value0", (source.kind.resource.kind).to_string())],
                    )
                    .to_string(),
                );
                continue;
            }
            let key = WatchKey::all(source.kind.resource.clone())
                .in_namespace(self.object.metadata.namespace.clone());
            let session = self.session.clone();
            let requested = key.clone();
            let fetching = Bridge::global(cx).run_cancellable(async move {
                session
                    .list_objects(requested)
                    .await
                    .map_err(|error| error.user_message())
            });
            source._list = Some(cx.spawn_in(window, async move |view, cx| {
                let result = fetching.result().await;
                let _ = view.update_in(cx, |view, window, cx| {
                    let source = &mut view.sources[index];
                    source.listed = true;
                    match result {
                        Ok(Ok(objects)) => {
                            source.store.apply(Delta::Reset(objects));
                            if source.kind.supports("watch") {
                                source._watch = Some(drain_into(
                                    cx,
                                    view.session.subscribe(key),
                                    move |view, batch, _, cx| {
                                        if view.sources[index].store.apply_batch(batch) {
                                            view.rebuild(cx);
                                        }
                                    },
                                    window,
                                ));
                            }
                        }
                        Ok(Err(error)) => {
                            source.error = Some(format!("{}: {error}", source.kind.resource.kind))
                        }
                        Err(error) => {
                            source.error = Some(format!("{}: {error}", source.kind.resource.kind))
                        }
                    }
                    view.rebuild(cx);
                });
            }));
        }
        self.rebuild(cx);
    }
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let object = self.object.clone();
        let kind = self.kind.clone();
        let search = self.search.read(cx).value().to_lowercase();
        let sources = self
            .sources
            .iter()
            .map(|source| {
                (
                    source.kind.clone(),
                    source
                        .store
                        .iter()
                        .map(|(_, object)| object.clone())
                        .collect::<Vec<_>>(),
                    source.listed && source.error.is_none(),
                )
            })
            .collect::<Vec<_>>();
        self.preparing = true;
        self._rebuild = Some(cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(30))
                .await;
            let rows = cx
                .background_executor()
                .spawn(async move { build_rows(&kind, &object, sources, &search) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.rows = rows;
                view.list.reset(view.rows.len());
                view.preparing = false;
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn render_row(&self, index: usize, cx: &Context<Self>) -> AnyElement {
        let row = &self.rows[index];
        let missing = row.absent || !row.missing.is_empty();
        let required_missing = (row.absent
            && row.references.iter().any(|reference| !reference.optional))
            || row.references.iter().any(|reference| {
                !reference.optional
                    && reference
                        .key
                        .as_ref()
                        .is_some_and(|key| row.missing.contains(key))
            });
        let target = row.target.clone();
        let kind = row.kind.clone();
        v_flex()
            .id(("configuration-consumer", index))
            .test_support()
            .w_full()
            .p_3()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new(("configuration-target", index))
                            .ghost()
                            .small()
                            .label(format!("{} · {}", row.label, row.target.name))
                            .text_color(cx.theme().resource_link())
                            .disabled(row.kind.is_none() || row.absent)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if let Some(kind) = kind.clone() {
                                    cx.emit(OwnerRequested {
                                        kind,
                                        target: target.clone(),
                                    });
                                }
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(row.target.namespace.clone().unwrap_or_default()),
                    )
                    .child(div().flex_1())
                    .when(missing, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(if required_missing {
                                    cx.theme().danger
                                } else {
                                    cx.theme().warning
                                })
                                .child(crate::i18n::text(if required_missing {
                                    "Missing reference"
                                } else {
                                    "Optional reference absent"
                                })),
                        )
                    })
                    .when(row.unchecked, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(crate::i18n::text("Not checked")),
                        )
                    }),
            )
            .when(row.absent, |view| {
                view.child(copyable_text(
                    ("configuration-missing", index),
                    crate::i18n::message(
                        "messages.was_not_found_in_this_namespace",
                        &[
                            ("value0", (row.label).to_string()),
                            ("value1", (row.target.name).to_string()),
                        ],
                    )
                    .to_string(),
                ))
            })
            .children(
                row.references
                    .iter()
                    .enumerate()
                    .map(|(reference_index, reference)| {
                        let missing = reference
                            .key
                            .as_ref()
                            .is_some_and(|key| row.missing.contains(key));
                        v_flex()
                            .gap_1()
                            .pl_2()
                            .border_l_2()
                            .border_color(if missing {
                                cx.theme().warning
                            } else {
                                cx.theme().border
                            })
                            .child(div().text_xs().child(copyable_text(
                                (
                                    SharedString::from(format!("usage-{index}-{reference_index}")),
                                    0usize,
                                ),
                                reference.description(),
                            )))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(copyable_text(
                                        (
                                            SharedString::from(format!(
                                                "usage-field-{index}-{reference_index}"
                                            )),
                                            0usize,
                                        ),
                                        reference.field.clone(),
                                    )),
                            )
                            .when(missing, |view| {
                                view.child(div().text_xs().text_color(cx.theme().warning).child(
                                    crate::i18n::text(if reference.optional {
                                        "Optional key is absent"
                                    } else {
                                        "Key is missing"
                                    }),
                                ))
                            })
                    }),
            )
            .into_any_element()
    }
}

fn build_rows(
    kind: &Kind,
    object: &DynamicObject,
    sources: Vec<(Arc<Kind>, Vec<Arc<DynamicObject>>, bool)>,
    search: &str,
) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(configuration) =
        ConfigurationKind::for_kind(&kind.resource.group, &kind.resource.kind)
    {
        for (kind, objects, checked) in sources {
            for consumer in objects {
                let references = config_refs::used_by(
                    configuration,
                    object,
                    &kind.resource.group,
                    &kind.resource.kind,
                    &consumer,
                );
                if references.is_empty() {
                    continue;
                }
                let missing = references
                    .iter()
                    .filter(|reference| reference.missing_key(object))
                    .filter_map(|reference| reference.key.clone())
                    .collect();
                rows.push(Row {
                    kind: Some(kind.clone()),
                    target: ObjectRef::of(&consumer),
                    label: kind.resource.kind.clone(),
                    references,
                    missing,
                    absent: false,
                    unchecked: !checked,
                });
            }
        }
    } else {
        let references = config_refs::references(&kind.resource.group, &kind.resource.kind, object);
        let mut groups = std::collections::BTreeMap::new();
        for reference in references {
            groups
                .entry((reference.kind, reference.name.clone()))
                .or_insert_with(Vec::new)
                .push(reference);
        }
        for ((config_kind, name), references) in groups {
            let source = sources.iter().find(|(kind, _, _)| {
                ConfigurationKind::for_kind(&kind.resource.group, &kind.resource.kind)
                    == Some(config_kind)
            });
            let found = source.and_then(|(_, objects, _)| {
                objects.iter().find(|candidate| {
                    candidate.metadata.namespace == object.metadata.namespace
                        && candidate.metadata.name.as_deref() == Some(name.as_str())
                })
            });
            let checked = source.is_some_and(|(_, _, checked)| *checked);
            let missing = found
                .map(|object| {
                    references
                        .iter()
                        .filter(|reference| reference.missing_key(object))
                        .filter_map(|reference| reference.key.clone())
                        .collect()
                })
                .unwrap_or_default();
            rows.push(Row {
                kind: source.map(|(kind, _, _)| kind.clone()),
                target: ObjectRef::new(object.metadata.namespace.clone(), name),
                label: config_kind.name().into(),
                references,
                missing,
                absent: checked && found.is_none(),
                unchecked: !checked,
            });
        }
    }
    rows.retain(|row| {
        search.is_empty()
            || format!(
                "{} {} {} {}",
                row.label,
                row.target,
                row.references
                    .iter()
                    .map(Reference::description)
                    .collect::<Vec<_>>()
                    .join(" "),
                row.references
                    .iter()
                    .map(|r| r.field.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .to_lowercase()
            .contains(search)
    });
    rows.sort_by(|a, b| a.label.cmp(&b.label).then(a.target.cmp(&b.target)));
    rows
}

impl Render for ConfigurationUsageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.preparing || self.sources.iter().any(|source| !source.listed);
        let by = ConfigurationKind::for_kind(&self.kind.resource.group, &self.kind.resource.kind)
            .is_some();
        v_flex().size_full().min_size_0().bg(cx.theme().background).text_color(cx.theme().foreground)
            .child(v_flex().p_3().gap_2().flex_shrink_0().border_b_1().border_color(cx.theme().border)
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text(if by {"Workloads and Pods that reference this configuration. Values are not shown."} else {"Configuration referenced by this resource; missing resources and keys are shown below."})))
                .child(h_flex().gap_2().items_center().child(div().flex_1().min_w_0().child(Input::new(&self.search).small()))
                    .child(Button::new("refresh-configuration-usage").ghost().small().label(crate::i18n::text("Refresh")).disabled(loading).on_click(cx.listener(|view,_,window,cx|view.load(window,cx))))))
            .children(self.sources.iter().enumerate().filter_map(|(index,source)| source.error.as_ref().map(|error|div().id(("configuration-usage-error-row",index)).test_support().px_3().py_2().text_xs().text_color(cx.theme().warning).child(copyable_text(("configuration-usage-error",index),error.clone())))))
            .child(div().px_3().py_2().text_xs().text_color(cx.theme().muted_foreground).child(if loading {"Checking references…".into()}else{format!("{} resources · {} references",self.rows.len(),self.rows.iter().map(|row|row.references.len()).sum::<usize>())}))
            .child(div().flex_1().min_size_0().when(self.rows.is_empty(),|view| view.child(div().id("configuration-usage-empty").test_support().p_3().text_sm().text_color(cx.theme().muted_foreground).child(crate::i18n::text(if loading {"Loading usage…"} else if self.sources.iter().any(|source|source.error.is_some()) {"Usage could not be fully checked. Review the errors above."} else if self.search.read(cx).value().is_empty() {"No configuration references found."}else{"No references match the search."}))))
                .when(!self.rows.is_empty(),|view|view.child(list(self.list.clone(),cx.processor(|view,index,_,cx|view.render_row(index,cx))).size_full())))
    }
}

struct UsageWindow {
    view: Entity<ConfigurationUsageView>,
    title: String,
}
impl Render for UsageWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(|_: &crate::app::CloseTab, window, _| window.remove_window())
            .child(
                TitleBar::new().child(div().text_sm().child(crate::i18n::title_text(&self.title))),
            )
            .child(div().flex_1().min_size_0().child(self.view.clone()))
    }
}
pub(crate) fn open(
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    object: Arc<DynamicObject>,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<Entity<ConfigurationUsageView>> {
    let bounds = Bounds::centered(
        window.display(cx).map(|display| display.id()),
        size(px(900.), px(660.)),
        cx,
    );
    let title = format!(
        "Used by · {} · {}",
        kind.resource.kind,
        ObjectRef::of(&object)
    );
    let (_, root) = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(560.), px(420.))),
            ..TitleBar::window_options()
        },
        cx,
        move |window, cx| {
            #[cfg(all(test, feature = "ui-tests"))]
            window.set_view_retention(false);
            crate::i18n::window_title(&title, window, cx);
            let view = cx.new(|cx| ConfigurationUsageView::new(session, kind, object, window, cx));
            cx.new(|_| UsageWindow { view, title })
        },
    )?;
    Ok(root.read(cx).view.clone())
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    use std::time::Duration;

    fn settle(
        cx: &mut TestAppContext,
        view: &Entity<ConfigurationUsageView>,
        ready: impl Fn(&ConfigurationUsageView) -> bool,
    ) {
        for _ in 0..200 {
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(35));
            cx.run_until_parked();
            if view.read_with(cx, |view, _| {
                !view.preparing && view.sources.iter().all(|source| source.listed) && ready(view)
            }) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            view.read_with(cx, |view, _| ready(view)),
            "configuration usage did not settle"
        );
    }
    fn configuration() -> serde_json::Value {
        json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"settings","namespace":"default","uid":"settings"},"data":{"present":"not-for-display"}})
    }
    fn pod(key: &str) -> serde_json::Value {
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"consumer","namespace":"default","uid":"consumer"},"spec":{"containers":[{"name":"app","env":[{"name":"VALUE","valueFrom":{"configMapKeyRef":{"name":"settings","key":key}}}]}]}})
    }

    #[::core::prelude::v1::test]
    fn usage_loads_live_references_missing_keys_search_and_resource_navigation() {
        let cx = &mut support::context();
        let config = configuration();
        let (fixture, session) =
            support::fixture(cx, "used-by", vec![config.clone(), pod("missing")]);
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    ConfigurationUsageView::new(
                        session,
                        Arc::new(support::kind("", "ConfigMap", "configmaps")),
                        Arc::new(serde_json::from_value(config).unwrap()),
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        settle(cx, &view, |view| view.rows.len() == 1);
        view.read_with(cx, |view, _| {
            assert!(view.rows[0].missing.contains("missing"));
            assert_eq!(view.rows[0].references[0].container.as_deref(), Some("app"));
        });
        let clicked = Arc::new(std::sync::Mutex::new(None));
        let navigation = clicked.clone();
        let _navigation = cx.update(|cx| {
            cx.subscribe(&view, move |_, event: &OwnerRequested, _| {
                *navigation.lock().unwrap() =
                    Some((event.kind.resource.kind.clone(), event.target.clone()));
            })
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("configuration-target", 0usize), cx);
        })
        .unwrap();
        assert_eq!(
            *clicked.lock().unwrap(),
            Some((
                "Pod".into(),
                ObjectRef::new(Some("default".into()), "consumer")
            ))
        );
        fixture.put(pod("present"));
        settle(cx, &view, |view| {
            view.rows.len() == 1 && view.rows[0].missing.is_empty()
        });
        let search = view.read_with(cx, |view, _| view.search.clone());
        cx.update_window(window, |_, window, cx| {
            search.read(cx).focus_handle(cx).focus(window, cx);
            window.input("does-not-exist", cx);
        })
        .unwrap();
        settle(cx, &view, |view| view.rows.is_empty());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("configuration-usage-empty").visible());
        })
        .unwrap();
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(line, _)| line.starts_with("GET"))
        );
    }

    #[::core::prelude::v1::test]
    fn workload_reports_missing_resources_and_denied_checks_as_unknown() {
        let cx = &mut support::context();
        let object = pod("missing");
        let (fixture, session) = support::fixture(cx, "configuration", vec![object.clone()]);
        fixture.deny("secrets");
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    ConfigurationUsageView::new(
                        session,
                        Arc::new(support::kind("", "Pod", "pods")),
                        Arc::new(serde_json::from_value(object).unwrap()),
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        settle(cx, &view, |view| view.rows.len() == 1);
        view.read_with(cx, |view, _| {
            assert!(view.rows[0].absent);
            assert!(!view.rows[0].unchecked);
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(view.read(cx).rows[0].absent);
            assert!(
                window
                    .find(("configuration-usage-error-row", 1usize))
                    .visible()
            );
        })
        .unwrap();
        let config: Arc<DynamicObject> = Arc::new(serde_json::from_value(configuration()).unwrap());
        let config_kind = Arc::new(support::kind("", "ConfigMap", "configmaps"));
        let kind = support::kind("", "Pod", "pods");
        let object: DynamicObject = serde_json::from_value(pod("missing")).unwrap();
        let rows = build_rows(
            &kind,
            &object,
            vec![(config_kind.clone(), vec![config.clone()], false)],
            "",
        );
        assert!(rows[0].unchecked);
        assert!(!rows[0].absent);
        let rows = build_rows(&kind, &object, vec![(config_kind, vec![config], true)], "");
        assert!(rows[0].missing.contains("missing"));
    }

    #[::core::prelude::v1::test]
    fn data_editing_and_delete_confirmation_offer_usage_without_sending_writes() {
        use crate::{
            detail::{DetailTab, DetailView},
            prompt::{Ask, Prompt, PromptEvent},
        };
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let config = configuration();
        let (fixture, session) =
            support::fixture(cx, "usage-entry", vec![config.clone(), pod("present")]);
        let (window, detail) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    let mut view = DetailView::new(
                        session.clone(),
                        Arc::new(support::kind("", "ConfigMap", "configmaps")),
                        Arc::new(serde_json::from_value(config.clone()).unwrap()),
                        None,
                        window,
                        cx,
                    );
                    view.select(DetailTab::Overview, window, cx);
                    view
                })
            })
            .unwrap()
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("data-view-usage", cx);
            window.render_frame(cx);
            assert!(window.find("refresh-configuration-usage").visible());
            detail.update(cx, |view, cx| view.select(DetailTab::Yaml, window, cx));
        })
        .unwrap();
        support::render_until(cx, window, |window| {
            window
                .try_find("yaml-view-usage")
                .is_some_and(|button| button.visible())
        });
        let (delete_window, prompt) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    Prompt::new(
                        Ask {
                            operation: beacon_kube::Operation::Delete,
                            target: ObjectRef::new(Some("default".into()), "settings"),
                            kind: "ConfigMap".into(),
                            bulk_targets: None,
                        },
                        window,
                        cx,
                    )
                    .with_usage(true)
                })
            })
            .unwrap()
        });
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = events.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&prompt, move |_, event: &PromptEvent, _| match event {
                PromptEvent::UsageRequested(target) => {
                    recorded.lock().unwrap().push(format!("usage {target}"))
                }
                PromptEvent::Confirmed(_) => recorded.lock().unwrap().push("confirmed".into()),
                PromptEvent::Cancelled => recorded.lock().unwrap().push("cancelled".into()),
            })
        });
        cx.update_window(delete_window, |_, window, cx| {
            window.render_frame(cx);
            window.click("delete-view-usage", cx);
            window.click("cancel", cx);
        })
        .unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec!["usage default/settings", "cancelled"]
        );
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(line, _)| line.starts_with("GET"))
        );
    }
}
