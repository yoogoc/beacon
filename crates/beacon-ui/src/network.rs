//! Live network paths; every available hop navigates to the actual resource.
use crate::{
    bridge::{Bridge, drain_into},
    copyable_text::copyable_text,
    detail::OwnerRequested,
};
use beacon_kube::{
    ClusterSession, Delta, DynamicObject, Kind, ResourceStore, WatchKey, network::Path,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

const DEPENDENCIES: [(&str, &str); 4] = [
    ("networking.k8s.io", "Ingress"),
    ("", "Service"),
    ("discovery.k8s.io", "EndpointSlice"),
    ("", "Pod"),
];
pub(crate) fn supported(group: &str, kind: &str) -> bool {
    DEPENDENCIES.contains(&(group, kind))
}

pub(crate) struct NetworkView {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    object: Arc<DynamicObject>,
    stores: [ResourceStore; 4],
    loading: [bool; 4],
    errors: [Option<String>; 4],
    paths: Arc<Vec<Path>>,
    scroll: UniformListScrollHandle,
    _lists: Vec<Task<()>>,
    _watches: Vec<Task<()>>,
}
impl EventEmitter<OwnerRequested> for NetworkView {}

impl NetworkView {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        object: Arc<DynamicObject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            session,
            kind,
            object,
            stores: std::array::from_fn(|_| ResourceStore::new()),
            loading: [false; 4],
            errors: std::array::from_fn(|_| None),
            paths: Arc::new(vec![]),
            scroll: UniformListScrollHandle::new(),
            _lists: vec![],
            _watches: vec![],
        };
        view.load(window, cx);
        view
    }
    pub(crate) fn refresh(&mut self, object: Arc<DynamicObject>, cx: &mut Context<Self>) {
        self.object = object;
        self.rebuild(cx);
    }
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        if self.loading.iter().any(|loading| *loading) || self.errors.iter().any(Option::is_some) {
            self.paths = Arc::new(vec![]);
        } else {
            self.paths = Arc::new(beacon_kube::network::paths(
                &self.kind.resource.group,
                &self.kind.resource.kind,
                self.object.clone(),
                &self.stores,
            ));
        }
        cx.notify();
    }
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._lists.clear();
        self._watches.clear();
        self.stores = std::array::from_fn(|_| ResourceStore::new());
        self.loading = [true; 4];
        self.errors = std::array::from_fn(|_| None);
        self.paths = Arc::new(vec![]);
        for (index, (group, name)) in DEPENDENCIES.into_iter().enumerate() {
            let Some(kind) = self
                .session
                .discovery()
                .kinds()
                .iter()
                .find(|kind| {
                    kind.resource.group == group
                        && kind.resource.kind == name
                        && kind.supports("list")
                })
                .cloned()
            else {
                self.loading[index] = false;
                if index != 0 {
                    self.errors[index] = Some(format!(
                        "{name} is not available for listing in this cluster."
                    ));
                }
                continue;
            };
            let key =
                WatchKey::all(kind.resource).in_namespace(self.object.metadata.namespace.clone());
            let requested = key.clone();
            let session = self.session.clone();
            let fetching = Bridge::global(cx).run_cancellable(async move {
                session
                    .list_objects(requested)
                    .await
                    .map_err(|error| error.user_message())
            });
            self._lists.push(cx.spawn_in(window, async move |this, cx| {
                let result = fetching.result().await;
                let _ = this.update_in(cx, |view, window, cx| {
                    view.loading[index] = false;
                    match result {
                        Ok(Ok(objects)) => {
                            view.stores[index].apply(Delta::Reset(objects));
                            view._watches.push(drain_into(
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
                        Ok(Err(error)) => view.errors[index] = Some(format!("{name}: {error}")),
                        Err(error) => view.errors[index] = Some(format!("{name}: {error}")),
                    }
                    view.rebuild(cx);
                });
            }));
        }
        self.rebuild(cx);
    }
}

impl Render for NetworkView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paths = self.paths.clone();
        let view = cx.weak_entity();
        let list = uniform_list("network-paths", paths.len(), move |range, _, cx| {
            range
                .filter_map(|index| {
                    paths.get(index).map(|path| {
                        let notes = path.notes.clone();
                        v_flex()
                            .h(px(168.))
                            .min_w(px(900.))
                            .p_3()
                            .gap_2()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(copyable_text(("network-route", index), path.route.clone()))
                            .child(h_flex().gap_2().children(path.hops.iter().enumerate().map(
                                |(stage, hop)| {
                                    let mut card = v_flex()
                                        .w(px(195.))
                                        .gap_1()
                                        .p_2()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(DEPENDENCIES[stage].1),
                                        );
                                    if let Some(hop) = hop {
                                        let object = hop.object.clone();
                                        let group = hop.group;
                                        let name = hop.kind;
                                        let view = view.clone();
                                        card = card.child(
                                            Button::new(SharedString::from(format!(
                                                "network-hop-{index}-{stage}"
                                            )))
                                            .small()
                                            .ghost()
                                            .label(hop.name.clone())
                                            .disabled(object.is_none())
                                            .on_click(move |_, _, cx| {
                                                if let Some(object) = &object {
                                                    let _ = view.update(cx, |view, cx| {
                                                        if let Some(kind) = view
                                                            .session
                                                            .discovery()
                                                            .kinds()
                                                            .iter()
                                                            .find(|kind| {
                                                                kind.resource.group == group
                                                                    && kind.resource.kind == name
                                                            })
                                                            .cloned()
                                                        {
                                                            cx.emit(OwnerRequested {
                                                                kind: Arc::new(kind),
                                                                target: beacon_kube::ObjectRef::of(
                                                                    object,
                                                                ),
                                                            });
                                                        }
                                                    });
                                                }
                                            }),
                                        );
                                    } else {
                                        card = card.child(div().text_sm().child("—"));
                                    }
                                    h_flex()
                                        .gap_2()
                                        .child(card)
                                        .when(stage < 3, |row| row.child("→"))
                                },
                            )))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .h(px(32.))
                                            .overflow_hidden()
                                            .text_xs()
                                            .child(copyable_text(
                                                ("network-notes", index),
                                                path.notes.join(" · "),
                                            )),
                                    )
                                    .when(!notes.is_empty(), |row| {
                                        row.child(
                                            Popover::new(("network-notes-detail", index))
                                                .trigger(
                                                    Button::new(("network-notes-button", index))
                                                        .xsmall()
                                                        .ghost()
                                                        .label("Details…"),
                                                )
                                                .content(move |_, _, _| {
                                                    div()
                                                        .id(("network-full-notes", index))
                                                        .w(px(480.))
                                                        .max_h(px(360.))
                                                        .overflow_y_scroll()
                                                        .child(v_flex().gap_2().children(
                                                            notes.iter().enumerate().map(
                                                                |(i, note)| {
                                                                    copyable_text(
                                                                        ("network-full-note", i),
                                                                        note.clone(),
                                                                    )
                                                                },
                                                            ),
                                                        ))
                                                }),
                                        )
                                    }),
                            )
                            .into_any_element()
                    })
                })
                .collect()
        })
        .with_width_from_item((!self.paths.is_empty()).then_some(0))
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .track_scroll(&self.scroll)
        .size_full()
        .pb_3();
        v_flex().size_full().gap_2()
            .child(h_flex().p_3().gap_2().child(div().flex_1().child("Ingress → Service → EndpointSlice → Pod"))
                .child(Button::new("refresh-network").small().ghost().label("Refresh").on_click(cx.listener(|view,_,window,cx|view.load(window,cx)))))
            .child(div().px_3().text_xs().text_color(cx.theme().muted_foreground).child("Configured resource relationships. Click a hop to inspect it; this view does not test live network connectivity."))
            .when(self.loading.iter().any(|loading| *loading),|view|view.child(div().px_3().child("Loading network resources…")))
            .children(self.errors.iter().enumerate().filter_map(|(i,error)|error.as_ref().map(|error|div().px_3().child(copyable_text(("network-load-error",i),format!("Network view is incomplete. {error}"))))))
            .child(div().flex_1().min_size_0().relative().child(list).child(Scrollbar::horizontal(&self.scroll).mode(ScrollbarMode::Always)).child(Scrollbar::vertical(&self.scroll)))
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    #[::core::prelude::v1::test]
    fn live_network_paths_navigate_to_the_referenced_pod() {
        let cx = &mut support::context();
        let service = json!({"apiVersion":"v1","kind":"Service","metadata":{"name":"web","namespace":"default","uid":"svc","resourceVersion":"1"},"spec":{"selector":{"app":"web"},"ports":[{"port":80,"targetPort":"http"}]}});
        let slice = json!({"apiVersion":"discovery.k8s.io/v1","kind":"EndpointSlice","metadata":{"name":"web-slice","namespace":"default","uid":"slice","resourceVersion":"1","labels":{"kubernetes.io/service-name":"web"}},"endpoints":[{"addresses":["10.0.0.1"],"conditions":{"ready":true},"targetRef":{"kind":"Pod","name":"web-pod","namespace":"default","uid":"pod"}}]});
        let pod = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web-pod","namespace":"default","uid":"pod","resourceVersion":"1","labels":{"app":"web"}},"spec":{"containers":[{"name":"app","ports":[{"name":"http","containerPort":8080}]}]}});
        let (fixture, session) = support::fixture(
            cx,
            "network-fixture",
            vec![service.clone(), slice, pod.clone()],
        );
        let selected = Arc::new(std::sync::Mutex::new(None));
        let selection = selected.clone();
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    NetworkView::new(
                        session,
                        Arc::new(support::kind("", "Service", "services")),
                        Arc::new(serde_json::from_value(service).unwrap()),
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        cx.update(|cx| {
            cx.subscribe(&view, move |_, event: &OwnerRequested, _| {
                *selection.lock().unwrap() = Some(event.target.clone());
            })
            .detach();
        });
        support::settle(cx, |cx| {
            fixture.watchers() >= 4 && view.read_with(cx, |view, _| !view.paths.is_empty())
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("network-hop-0-3", cx);
        })
        .unwrap();
        assert_eq!(selected.lock().unwrap().as_ref().unwrap().name, "web-pod");
        let mut replacement = pod;
        replacement["metadata"]["uid"] = json!("replaced");
        replacement["metadata"]["resourceVersion"] = json!("2");
        fixture.put(replacement);
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.paths[0]
                    .notes
                    .iter()
                    .any(|note| note.contains("UID has changed"))
            })
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("refresh-network").visible());
        })
        .unwrap();
    }
}
