//! Live network paths; every available hop navigates to the actual resource.
use crate::{
    bridge::{Bridge, drain_into},
    copyable_text::copyable_text,
    detail::OwnerRequested,
};
use beacon_kube::{
    ClusterSession, Delta, DynamicObject, Kind, ResourceStore, WatchKey, network::Path,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::Cell, rc::Rc, sync::Arc};

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
    width: Rc<Cell<Pixels>>,
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
            width: Rc::new(Cell::new(px(720.))),
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
                    self.errors[index] = Some(
                        crate::i18n::message(
                            "messages.name_is_not_available_for_listing_in_this_cluster",
                            &[("name", (name).to_string())],
                        )
                        .to_string(),
                    );
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
        let owner = cx.weak_entity();
        let target = beacon_kube::ObjectRef::of(&self.object);
        let width = self.width.get();
        let columns = if width >= px(1000.) {
            4
        } else if width >= px(440.) {
            2
        } else {
            1
        };
        let card_width =
            ((width - px(32.) - px(8.) * (columns - 1) as f32) / columns as f32).max(px(100.));
        let row_height = px(98. + 64. * (4 / columns) as f32);
        let icons = [
            IconName::Network,
            IconName::RadioTower,
            IconName::Layers,
            IconName::Box,
        ];
        let list = uniform_list("network-paths", paths.len(), move |range, _, cx| {
            range
                .filter_map(|index| {
                    paths.get(index).map(|path| {
                        let notes = path.notes.clone();
                        v_flex()
                            .h(row_height)
                            .min_w_0()
                            .w_full()
                            .p_3()
                            .gap_2()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .h(px(28.))
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(copyable_text(
                                        ("network-route", index),
                                        path.route.clone(),
                                    )),
                            )
                            .child(h_flex().gap_2().flex_wrap().items_start().children(
                                path.hops.iter().enumerate().map(|(stage, hop)| {
                                    let selected = hop
                                        .as_ref()
                                        .and_then(|hop| hop.object.as_ref())
                                        .is_some_and(|object| {
                                            beacon_kube::ObjectRef::of(object) == target
                                        });
                                    let mut card = v_flex()
                                        .w(card_width)
                                        .h(px(56.))
                                        .min_w_0()
                                        .px_2()
                                        .py_1()
                                        .gap_0p5()
                                        .rounded_md()
                                        .bg(cx.theme().muted.opacity(0.4))
                                        .border_1()
                                        .border_color(if selected {
                                            cx.theme().primary.opacity(0.5)
                                        } else {
                                            cx.theme().border.opacity(0.7)
                                        })
                                        .child(
                                            h_flex()
                                                .gap_1p5()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(Icon::new(icons[stage]).size(px(12.)))
                                                .child(crate::i18n::text(DEPENDENCIES[stage].1))
                                                .child(div().flex_1())
                                                .when(stage < 3, |row| {
                                                    row.child(
                                                        Icon::new(if (stage + 1) % columns == 0 {
                                                            IconName::ArrowDown
                                                        } else {
                                                            IconName::ArrowRight
                                                        })
                                                        .size(px(12.)),
                                                    )
                                                }),
                                        );
                                    if let Some(hop) = hop {
                                        let object = hop.object.clone();
                                        let group = hop.group;
                                        let name = hop.kind;
                                        let owner = owner.clone();
                                        card = card.child(
                                            Button::new(SharedString::from(format!(
                                                "network-hop-{index}-{stage}"
                                            )))
                                            .xsmall()
                                            .ghost()
                                            .w_full()
                                            .label(hop.name.clone())
                                            .disabled(object.is_none())
                                            .tooltip(hop.name.clone())
                                            .on_click(move |_, _, cx| {
                                                if let Some(object) = &object {
                                                    let _ = owner.update(cx, |view, cx| {
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
                                        card = card.child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("—"),
                                        );
                                    }
                                    card
                                }),
                            ))
                            .child(
                                h_flex()
                                    .h(px(22.))
                                    .min_w_0()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
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
                                                        .icon(IconName::Info)
                                                        .tooltip(crate::i18n::text("Details")),
                                                )
                                                .content(move |_, _, _| {
                                                    div()
                                                        .id(("network-full-notes", index))
                                                        .w(px(360.))
                                                        .max_h(px(280.))
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
        .track_scroll(&self.scroll)
        .size_full();
        let measured = self.width.clone();
        let entity = cx.entity_id();
        v_flex().id("network-path-view").test_support().size_full().min_size_0().overflow_hidden()
            .on_prepaint(move |bounds, _, cx| { if measured.replace(bounds.size.width) != bounds.size.width { cx.notify(entity); } })
            .child(h_flex().px_3().py_2().gap_2().flex_shrink_0().border_b_1().border_color(cx.theme().border)
                .child(Icon::new(IconName::Network).size(px(16.)).text_color(cx.theme().muted_foreground))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(crate::i18n::text("Network path")))
                .child(div().flex_1().text_xs().text_color(cx.theme().muted_foreground).child(self.paths.len().to_string()))
                .child(Button::new("network-path-info").xsmall().ghost().icon(IconName::Info)
                    .tooltip(crate::i18n::text("Configured resource relationships. Click a hop to inspect it; this view does not test live network connectivity.")))
                .child(Button::new("refresh-network").xsmall().ghost().icon(IconName::RefreshCw).tooltip(crate::i18n::text("Refresh"))
                    .on_click(cx.listener(|view,_,window,cx|view.load(window,cx)))))
            .when(self.loading.iter().any(|loading| *loading), |view| view.child(h_flex().p_3().gap_2()
                .child(gpui_kit::component::spinner::Spinner::new().small())
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(crate::i18n::text("Loading network resources…")))))
            .when(!self.loading.iter().any(|loading|*loading) && self.paths.is_empty() && self.errors.iter().all(Option::is_none), |view| view.child(div().p_4().text_sm().text_color(cx.theme().muted_foreground).child(crate::i18n::text("No network paths found"))))
            .children(self.errors.iter().enumerate().filter_map(|(i,error)|error.as_ref().map(|error|div().p_3().text_sm().text_color(cx.theme().danger)
                .child(copyable_text(("network-load-error",i),crate::i18n::message("messages.network_view_is_incomplete_error", &[("error", error.clone())]).to_string())))))
            .child(div().flex_1().min_size_0().relative().child(list).child(Scrollbar::vertical(&self.scroll)))
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
        for width in [400., 640., 1080.] {
            cx.simulate_window_resize(window, size(px(width), px(800.)));
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                for stage in 1..4 {
                    let hop = window.find(SharedString::from(format!("network-hop-0-{stage}")));
                    assert!(hop.visible());
                    assert!(
                        hop.bounds().left() >= px(0.) && hop.bounds().right() <= px(width),
                        "path stage {stage} should fit a {width}px panel: {:?}",
                        hop.bounds()
                    );
                }
            })
            .unwrap();
        }
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
