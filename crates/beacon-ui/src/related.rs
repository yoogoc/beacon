//! Live, navigable resource relationships in their own detail tab.
use crate::{
    bridge::{Bridge, drain_into},
    detail::OwnerRequested,
    table::ResourceTable,
};
use beacon_columns::{ColumnSet, ColumnWidth};
use beacon_kube::{
    ClusterSession, Delta, DynamicObject, Kind, ResourceStore, WatchKey,
    relationships::Relationship,
};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use std::sync::Arc;

struct Source {
    kind: Option<Arc<Kind>>,
    name: &'static str,
    listed: bool,
    error: Option<String>,
    _list: Option<Task<()>>,
    _watch: Option<Task<()>>,
}

fn columns(group: &str, kind: &str) -> ColumnSet {
    let mut columns = ColumnSet::for_kind(group, kind, false);
    columns
        .columns
        .retain(|column| !matches!(column.header.as_str(), "CPU" | "Memory"));
    for column in &mut columns.columns {
        column.width = ColumnWidth::Fixed(match column.header.as_str() {
            "Name" => 190.,
            "Containers" => 96.,
            "Ready" => 64.,
            "Status" => 140.,
            _ => 80.,
        });
    }
    columns
}

pub(crate) struct RelatedView {
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    relationship: Relationship,
    sources: Vec<Source>,
    stores: Vec<ResourceStore>,
    tables: Vec<Entity<TableState<ResourceTable>>>,
    search: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
    _clock: Task<()>,
}
impl EventEmitter<OwnerRequested> for RelatedView {}

impl RelatedView {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        object: Arc<DynamicObject>,
        relationship: Relationship,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sources: Vec<_> = relationship
            .dependencies()
            .iter()
            .map(|(group, name)| {
                let kind = session
                    .discovery()
                    .kinds()
                    .iter()
                    .find(|kind| {
                        kind.resource.group == *group
                            && kind.resource.kind == *name
                            && kind.supports("list")
                    })
                    .cloned()
                    .map(Arc::new);
                Source {
                    kind,
                    name,
                    listed: false,
                    error: None,
                    _list: None,
                    _watch: None,
                }
            })
            .collect();
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search related resources")
                .clean_on_escape()
        });
        let tables: Vec<_> = relationship
            .dependencies()
            .iter()
            .map(|(group, kind)| {
                cx.new(|cx| {
                    TableState::new(
                        ResourceTable::new(columns(group, kind)).without_selection(),
                        window,
                        cx,
                    )
                })
            })
            .collect();
        let mut subscriptions = Vec::new();
        for (index, table) in tables.iter().enumerate() {
            subscriptions.push(
                cx.subscribe(table, move |view, table, event: &TableEvent, cx| {
                    if let TableEvent::SelectRow(row) = event
                        && let Some(kind) = view.sources[index].kind.clone()
                        && let Some(target) = table.read(cx).delegate().key_at(*row).cloned()
                    {
                        cx.emit(OwnerRequested { kind, target });
                    }
                }),
            );
        }
        subscriptions.push(
            cx.subscribe(&search, |view, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let search = input.read(cx).value().to_string();
                    for table in &view.tables {
                        table.update(cx, |table, cx| {
                            table.delegate_mut().set_filter(&search);
                            table.clear_selection(cx);
                            table.scroll_to_row(0, cx);
                            cx.notify();
                        });
                    }
                    cx.notify();
                }
            }),
        );
        let stores = (0..sources.len()).map(|_| ResourceStore::new()).collect();
        let mut view = Self {
            session,
            object,
            relationship,
            sources,
            stores,
            tables,
            search,
            _subscriptions: subscriptions,
            _clock: Task::ready(()),
        };
        view.load(window, cx);
        view._clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                if this
                    .update(cx, |view, cx| {
                        for table in &view.tables {
                            table.update(cx, |table, cx| {
                                table.delegate_mut().tick();
                                cx.notify();
                            });
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        view
    }

    pub(crate) fn refresh(&mut self, object: Arc<DynamicObject>, cx: &mut Context<Self>) {
        self.object = object;
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let groups = self.relationship.resolve(&self.object, &self.stores);
        for (table, objects) in self.tables.iter().zip(groups) {
            table.update(cx, |table, cx| {
                table.delegate_mut().apply(vec![Delta::Reset(objects)]);
                table.delegate_mut().finish_loading();
                cx.notify();
            });
        }
        cx.notify();
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for index in 0..self.sources.len() {
            self.sources[index]._watch = None;
            self.sources[index]._list = None;
            self.sources[index].listed = false;
            self.stores[index] = ResourceStore::new();
            let Some(kind) = self.sources[index].kind.clone() else {
                self.sources[index].listed = true;
                self.sources[index].error = Some(format!(
                    "{} is not available for listing in this cluster.",
                    self.sources[index].name
                ));
                continue;
            };
            self.sources[index].error = None;
            let key = WatchKey::all(kind.resource.clone())
                .in_namespace(self.object.metadata.namespace.clone());
            let session = self.session.clone();
            let requested = key.clone();
            let fetching = Bridge::global(cx).run_cancellable(async move {
                session
                    .list_objects(requested)
                    .await
                    .map_err(|error| error.user_message())
            });
            self.sources[index]._list = Some(cx.spawn_in(window, async move |this, cx| {
                let result = fetching.result().await;
                let _ = this.update_in(cx, |view, window, cx| {
                    view.sources[index].listed = true;
                    match result {
                        Ok(Ok(objects)) => {
                            view.stores[index].apply(Delta::Reset(objects));
                            view.sources[index]._watch = Some(drain_into(
                                cx,
                                view.session.subscribe(key),
                                move |view, batch, _, cx| {
                                    if view.stores[index].apply_batch(batch) {
                                        view.rebuild(cx);
                                    }
                                },
                                window,
                            ));
                        }
                        Ok(Err(error)) => {
                            view.sources[index].error =
                                Some(format!("{}: {error}", view.sources[index].name))
                        }
                        Err(error) => {
                            view.sources[index].error =
                                Some(format!("{}: {error}", view.sources[index].name))
                        }
                    }
                    view.rebuild(cx);
                });
            }));
        }
        self.rebuild(cx);
    }

    fn render_group(&self, index: usize, cx: &App) -> AnyElement {
        let dependencies = if index == 0 {
            &self.sources[..1]
        } else {
            &self.sources[..=index]
        };
        let loading = dependencies.iter().any(|source| !source.listed);
        let errors: Vec<_> = dependencies
            .iter()
            .filter_map(|source| source.error.clone())
            .collect();
        render_related_group(
            index,
            self.sources[index].name,
            &self.tables[index],
            loading,
            &errors,
            cx,
        )
    }
}

fn render_related_group(
    index: usize,
    source_name: &str,
    table_entity: &Entity<TableState<ResourceTable>>,
    loading: bool,
    errors: &[String],
    cx: &App,
) -> AnyElement {
    let table = table_entity.read(cx).delegate();
    let name = match source_name {
        "ReplicaSet" => "ReplicaSets",
        "Job" => "Jobs",
        "EndpointSlice" => "EndpointSlices",
        _ => "Pods",
    };
    let body = if !errors.is_empty() {
        div()
            .id(("related-error-state", index))
            .p_3()
            .text_sm()
            .text_color(cx.theme().danger)
            .child(crate::copyable_text::copyable_text(
                ("related-errors", index),
                errors.join("\n"),
            ))
            .test_support()
            .into_any_element()
    } else if loading {
        div()
            .id(("related-loading", index))
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(format!("Loading {name}…"))
            .test_support()
            .into_any_element()
    } else if table.is_empty() {
        div()
            .id(("related-empty", index))
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(if table.total() > 0 {
                "No related resources match the search."
            } else {
                "No related resources found."
            })
            .test_support()
            .into_any_element()
    } else {
        div()
            .id(("related-table", index))
            .size_full()
            .min_size_0()
            .child(table_entity.clone())
            .test_support()
            .into_any_element()
    };
    v_flex()
        .id(("related-group", index))
        .flex_1()
        .min_size_0()
        .child(
            h_flex()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .text_sm()
                .child(div().font_weight(FontWeight::MEDIUM).child(name))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if loading || !errors.is_empty() {
                            String::new()
                        } else {
                            format!("{} / {}", table.len(), table.total())
                        }),
                ),
        )
        .child(div().flex_1().min_size_0().overflow_hidden().child(body))
        .into_any_element()
}

impl Render for RelatedView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.sources.iter().any(|source| !source.listed);
        v_flex()
            .size_full()
            .min_size_0()
            .child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.relationship.description()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&self.search).small()),
                            )
                            .child(
                                Button::new("refresh-related-resources")
                                    .small()
                                    .ghost()
                                    .label("Refresh")
                                    .disabled(loading)
                                    .on_click(
                                        cx.listener(|view, _, window, cx| view.load(window, cx)),
                                    ),
                            ),
                    ),
            )
            .children((0..self.tables.len()).map(|index| self.render_group(index, cx)))
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod rendering_tests {
    use super::*;
    use beacon_kube::ObjectRef;
    use gpui_kit::test::TestWindowExt as _;

    struct Pane {
        table: Entity<TableState<ResourceTable>>,
        loading: bool,
        errors: Vec<String>,
        selected: Option<ObjectRef>,
        _selection: Subscription,
    }
    impl Render for Pane {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            v_flex().w(px(420.)).h(px(350.)).child(render_related_group(
                0,
                "Pod",
                &self.table,
                self.loading,
                &self.errors,
                cx,
            ))
        }
    }

    #[::core::prelude::v1::test]
    fn related_pane_renders_loading_errors_live_updates_and_row_navigation() {
        let cx = &mut TestAppContext::single();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let (window, pane) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                let table = cx.new(|cx| {
                    TableState::new(
                        ResourceTable::new(columns("", "Pod")).without_selection(),
                        window,
                        cx,
                    )
                });
                cx.new(|cx| {
                    let selection =
                        cx.subscribe(&table, |view: &mut Pane, table, event: &TableEvent, cx| {
                            if let TableEvent::SelectRow(row) = event {
                                view.selected = table.read(cx).delegate().key_at(*row).cloned();
                            }
                        });
                    Pane {
                        table,
                        loading: true,
                        errors: vec![],
                        selected: None,
                        _selection: selection,
                    }
                })
            })
            .unwrap()
        });
        let table = pane.read_with(cx, |pane, _| pane.table.clone());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(("related-loading", 0usize)).visible());
        })
        .unwrap();
        pane.update(cx, |pane, cx| {
            pane.loading = false;
            pane.errors = vec!["403 Forbidden: Pods cannot be listed".into()];
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(("related-error-state", 0usize)).visible());
        })
        .unwrap();
        pane.update(cx, |pane, cx| {
            pane.errors.clear();
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(("related-empty", 0usize)).visible());
        })
        .unwrap();
        for revision in 0..3 {
            let pod = Arc::new(serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod", "metadata":{"name":"consumer","namespace":"default","uid":"consumer","resourceVersion":revision.to_string()},"spec":{"containers":[{"name":"app"}]},"status":{"phase":"Running"}})).unwrap());
            table.update(cx, |table, cx| {
                table.delegate_mut().apply(vec![Delta::Reset(vec![pod])]);
                cx.notify();
            });
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert!(window.find(("related-table", 0usize)).visible());
                window.click_at(("row", 0usize), point(px(12.), px(10.)), cx);
            })
            .unwrap();
            assert_eq!(
                pane.read_with(cx, |pane, _| pane.selected.clone()),
                Some(ObjectRef::new(Some("default".into()), "consumer"))
            );
        }
        table.update(cx, |table, cx| {
            table.delegate_mut().set_filter("absent");
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(("related-empty", 0usize)).visible());
        })
        .unwrap();
    }
}
