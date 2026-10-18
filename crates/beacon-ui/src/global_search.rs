//! Connected-cluster search and persistent navigation favorites.
use crate::{
    bridge::Bridge, connections::SharedConnections, copyable_text::copyable_text, settings,
};
use beacon_kube::{ClusterId, Kind, ObjectRef};
use futures::{StreamExt as _, stream};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, TitleBar, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Favorite {
    pub cluster: String,
    pub group: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
}
impl Favorite {
    pub(crate) fn new(cluster: &ClusterId, kind: &Kind, target: &ObjectRef) -> Self {
        Self {
            cluster: cluster.as_str().into(),
            group: kind.resource.group.clone(),
            kind: kind.resource.kind.clone(),
            namespace: target.namespace.clone(),
            name: target.name.clone(),
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Favorites {
    pub clusters: BTreeSet<String>,
    pub resources: BTreeSet<Favorite>,
}
pub(crate) fn toggle_cluster(id: &ClusterId, cx: &mut App) -> Result<(), String> {
    let mut favorites = settings::store(cx).read(cx).preferences.favorites.clone();
    if !favorites.clusters.remove(id.as_str()) {
        favorites.clusters.insert(id.to_string());
    }
    settings::save_favorites(favorites, cx)
}
pub(crate) fn toggle_resource(favorite: Favorite, cx: &mut App) -> Result<(), String> {
    let mut favorites = settings::store(cx).read(cx).preferences.favorites.clone();
    if !favorites.resources.remove(&favorite) {
        favorites.resources.insert(favorite);
    }
    settings::save_favorites(favorites, cx)
}

#[derive(Clone)]
pub(crate) enum OpenRequested {
    Resource {
        cluster: ClusterId,
        kind: Arc<Kind>,
        target: ObjectRef,
    },
    Cluster(ClusterId),
}
#[derive(Clone)]
struct Entry {
    cluster: ClusterId,
    label: String,
    kind: Arc<Kind>,
    object: ObjectRef,
    searchable: String,
}
impl Entry {
    fn favorite(&self) -> Favorite {
        Favorite::new(&self.cluster, &self.kind, &self.object)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Search,
    Favorites,
}
pub(crate) struct GlobalSearch {
    search: Entity<InputState>,
    page: Page,
    entries: Arc<Vec<Entry>>,
    visible: Vec<usize>,
    list: ListState,
    loading: bool,
    errors: Vec<String>,
    save_error: Option<String>,
    _load: Option<Task<()>>,
    _filter: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<OpenRequested> for GlobalSearch {}
impl GlobalSearch {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            crate::i18n::input("Name, kind, namespace, cluster or label…", window, cx)
                .clean_on_escape()
        });
        let input = cx.subscribe(&search, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.filter(cx);
            }
        });
        let registry = cx.global::<SharedConnections>().0.clone();
        let connections = cx.subscribe_in(
            &registry,
            window,
            |view, _, _: &crate::connections::Changed, window, cx| view.load(window, cx),
        );
        let preferences = settings::store(cx);
        let saved = cx.observe(&preferences, |view, _, cx| {
            view.filter(cx);
            cx.notify();
        });
        let mut view = Self {
            search,
            page: Page::Search,
            entries: Arc::new(vec![]),
            visible: vec![],
            list: ListState::new(0, ListAlignment::Top, px(56.)),
            loading: false,
            errors: vec![],
            save_error: None,
            _load: None,
            _filter: None,
            _subscriptions: vec![input, connections, saved],
        };
        view.load(window, cx);
        view
    }
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._load = None;
        self.entries = Arc::new(vec![]);
        self.visible.clear();
        self.list.reset(0);
        self.errors.clear();
        self.filter(cx);
        let sessions: Vec<_> = cx
            .global::<SharedConnections>()
            .0
            .read(cx)
            .sessions
            .values()
            .cloned()
            .collect();
        let jobs: Vec<_> = sessions
            .into_iter()
            .flat_map(|session| {
                let label = settings::store(cx)
                    .read(cx)
                    .preferences
                    .clusters
                    .get(session.id().as_str())
                    .filter(|setting| !setting.alias.trim().is_empty())
                    .map(|setting| setting.alias.clone())
                    .unwrap_or_else(|| session.id().display_name().into());
                session
                    .discovery()
                    .kinds()
                    .iter()
                    .filter(|kind| kind.supports("list"))
                    .map(|kind| (session.clone(), label.clone(), Arc::new(kind.clone())))
                    .collect::<Vec<_>>()
            })
            .collect();
        self.loading = !jobs.is_empty();
        if jobs.is_empty() {
            cx.notify();
            return;
        }
        let loading = Bridge::global(cx).run_cancellable(async move {
            let results = stream::iter(jobs.into_iter().map(|(session, label, kind)| async move {
                beacon_kube::search::objects(&session, &kind)
                    .await
                    .map(|objects| {
                        objects
                            .into_iter()
                            .map(|object| {
                                let labels = object
                                    .labels
                                    .into_iter()
                                    .map(|(key, value)| format!("{key}={value}"))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                let searchable = format!(
                                    "{} {} {} {} {} {labels}",
                                    session.id(),
                                    label,
                                    kind.display_name(),
                                    kind.resource.group,
                                    object.reference
                                )
                                .to_lowercase();
                                Entry {
                                    cluster: session.id().clone(),
                                    label: label.clone(),
                                    kind: kind.clone(),
                                    object: object.reference,
                                    searchable,
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .map_err(|error| {
                        format!(
                            "{label} · {}: {}",
                            kind.display_name(),
                            error.user_message()
                        )
                    })
            }))
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
            let mut entries = Vec::new();
            let mut errors = Vec::new();
            for result in results {
                match result {
                    Ok(mut objects) => entries.append(&mut objects),
                    Err(error) => errors.push(error),
                }
            }
            entries.sort_by(|a, b| {
                a.cluster
                    .cmp(&b.cluster)
                    .then(a.kind.resource.kind.cmp(&b.kind.resource.kind))
                    .then(a.object.cmp(&b.object))
            });
            (Arc::new(entries), errors)
        });
        self._load = Some(cx.spawn_in(window, async move |view, cx| {
            let answer = loading.result().await;
            let _ = view.update(cx, |view, cx| {
                view.loading = false;
                match answer {
                    Ok((entries, errors)) => {
                        view.entries = entries;
                        view.errors = errors;
                    }
                    Err(error) => view.errors.push(error.to_string()),
                }
                view.filter(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_lowercase();
        let favorites = settings::store(cx).read(cx).preferences.favorites.clone();
        let page = self.page;
        let entries = self.entries.clone();
        self._filter = Some(cx.spawn(async move |view, cx| {
            let visible = cx
                .background_executor()
                .spawn(async move {
                    entries
                        .iter()
                        .enumerate()
                        .filter(|(_, entry)| {
                            (page == Page::Search
                                || favorites.resources.contains(&entry.favorite()))
                                && matches(&entry.searchable, &query)
                        })
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.visible = visible;
                view.list.reset(view.visible.len());
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn render_row(&self, row: usize, cx: &Context<Self>) -> AnyElement {
        let entry = &self.entries[self.visible[row]];
        let favorite = entry.favorite();
        let saved = settings::store(cx)
            .read(cx)
            .preferences
            .favorites
            .resources
            .contains(&favorite);
        let open = OpenRequested::Resource {
            cluster: entry.cluster.clone(),
            kind: entry.kind.clone(),
            target: entry.object.clone(),
        };
        h_flex()
            .id(("global-result", row))
            .test_support()
            .h(px(56.))
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new(("global-open", row))
                    .ghost()
                    .small()
                    .label(entry.object.name.clone())
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(open.clone()))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} · {} · {}",
                        entry.kind.display_name(),
                        entry.object.namespace.as_deref().unwrap_or("Cluster scope"),
                        entry.label
                    )),
            )
            .child(div().flex_1())
            .child(
                Button::new(("global-favorite", row))
                    .ghost()
                    .small()
                    .label(if saved { "★" } else { "☆" })
                    .accessibility_label(crate::i18n::text(if saved {
                        "Remove favorite"
                    } else {
                        "Add favorite"
                    }))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.save_error = toggle_resource(favorite.clone(), cx).err();
                        view.filter(cx);
                    })),
            )
            .into_any_element()
    }
    fn favorite_clusters(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let preferences = settings::store(cx).read(cx).preferences.clone();
        let catalog = crate::sources::store(cx).read(cx).catalog.clone();
        preferences
            .favorites
            .clusters
            .iter()
            .filter_map(|id| {
                let context = catalog
                    .contexts
                    .entries()
                    .iter()
                    .find(|context| context.id.as_str() == id)?;
                let id = context.id.clone();
                let label = preferences
                    .clusters
                    .get(id.as_str())
                    .filter(|settings| !settings.alias.is_empty())
                    .map_or_else(|| context.label(), |settings| settings.alias.clone());
                let connected = cx
                    .global::<SharedConnections>()
                    .0
                    .read(cx)
                    .sessions
                    .contains_key(&id);
                let remove = id.clone();
                Some(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .px_3()
                        .py_1()
                        .child(
                            Button::new(SharedString::from(format!("favorite-cluster-{id}")))
                                .ghost()
                                .small()
                                .label(label)
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(OpenRequested::Cluster(id.clone()))
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::text(if connected {
                                    "Connected"
                                } else {
                                    "Click to connect"
                                })),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new(SharedString::from(format!("remove-cluster-{remove}")))
                                .ghost()
                                .small()
                                .label("★")
                                .accessibility_label(crate::i18n::text("Remove favorite"))
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.save_error = toggle_cluster(&remove, cx).err();
                                    cx.notify();
                                })),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}
fn matches(candidate: &str, query: &str) -> bool {
    query
        .split_whitespace()
        .all(|term| candidate.contains(term))
}
impl Render for GlobalSearch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connected: BTreeSet<_> = cx
            .global::<SharedConnections>()
            .0
            .read(cx)
            .sessions
            .keys()
            .map(|id| id.to_string())
            .collect();
        let unavailable: Vec<_> = settings::store(cx)
            .read(cx)
            .preferences
            .favorites
            .resources
            .iter()
            .filter(|favorite| {
                !self
                    .entries
                    .iter()
                    .any(|entry| entry.favorite() == **favorite)
            })
            .cloned()
            .collect();
        v_flex().size_full().min_size_0().bg(cx.theme().background).text_color(cx.theme().foreground)
            .on_action(|_:&crate::app::CloseTab,window,_|window.remove_window())
            .child(TitleBar::new().child(crate::i18n::text("Search & favorites")))
            .child(v_flex().px_4().py_3().gap_2()
                .child(h_flex().gap_2().child(Button::new("search-page").small().ghost().label(crate::i18n::text("All resources")).selected(self.page==Page::Search).on_click(cx.listener(|view,_,_,cx|{view.page=Page::Search;view.filter(cx);})))
                    .child(Button::new("favorites-page").small().ghost().label(crate::i18n::text("Favorites")).selected(self.page==Page::Favorites).on_click(cx.listener(|view,_,_,cx|{view.page=Page::Favorites;view.filter(cx);})))
                    .child(div().flex_1()).child(Button::new("global-refresh").small().ghost().label(crate::i18n::text("Refresh")).disabled(self.loading).on_click(cx.listener(|view,_,window,cx|view.load(window,cx))))
                    .when(self.loading,|row|row.child(Button::new("global-cancel").small().ghost().label(crate::i18n::text("Cancel")).on_click(cx.listener(|view,_,_,cx|{view._load=None;view.loading=false;cx.notify();})))) )
                .child(Input::new(&self.search).small())
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(if self.loading {"Loading metadata from connected clusters…".into()}else{crate::i18n::message("messages.matches_connected_clusters_kinds_could_not_be_read", &[("value0", (self.visible.len()).to_string()), ("value1", (connected.len()).to_string()), ("value2", (self.errors.len()).to_string())]).to_string()})))
            .when(self.page==Page::Favorites,|view|view.children(self.favorite_clusters(cx)).children(unavailable.iter().enumerate().map(|(index,favorite)| {
                let remove=favorite.clone();
                h_flex().px_4().py_1().gap_2().text_xs().text_color(cx.theme().muted_foreground)
                    .child(div().flex_1().child(format!("{} · {} · {} · {} ({})",favorite.name,favorite.kind,favorite.namespace.as_deref().unwrap_or("Cluster scope"),favorite.cluster,if connected.contains(&favorite.cluster){if self.loading{"Loading"}else{"Unavailable"}}else{"Disconnected"})))
                    .child(Button::new(("remove-unavailable-favorite",index)).small().ghost().label(crate::i18n::text("Remove")).on_click(cx.listener(move|view,_,_,cx| {view.save_error=toggle_resource(remove.clone(),cx).err();cx.notify();})))
            })))
            .when_some(self.save_error.clone(),|view,error|view.child(copyable_text("favorite-save-error",error)))
            .when(!self.errors.is_empty(),|view|view.child(div().px_4().text_xs().text_color(cx.theme().warning).child(copyable_text("global-search-errors",self.errors.join("\n")))))
            .child(div().flex_1().min_size_0().when(self.visible.is_empty(),|view|view.child(div().id("global-empty").test_support().p_4().text_sm().text_color(cx.theme().muted_foreground).child(crate::i18n::text(if self.loading {"Searching…"}else if connected.is_empty(){"Connect a cluster to search its resources. Favorites are saved locally."}else{"No resources match. Try a name, kind, namespace or label."}))))
                .when(!self.visible.is_empty(),|view|view.child(list(self.list.clone(),cx.processor(|view,row,_,cx|view.render_row(row,cx))).size_full())))
    }
}
pub(crate) fn open(cx: &mut App) -> anyhow::Result<Entity<GlobalSearch>> {
    let mut child = None;
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(960.), px(680.)),
                cx,
            ))),
            window_min_size: Some(size(px(620.), px(400.))),
            ..TitleBar::window_options()
        },
        |window, cx| {
            crate::i18n::window_title("Beacon — Search & favorites", window, cx);
            let view = cx.new(|cx| GlobalSearch::new(window, cx));
            let root = cx.new(|cx| gpui_kit::component::Root::new(view.clone(), window, cx));
            child = Some(view);
            window.activate_window();
            root
        },
    )?;
    Ok(child.expect("window builder supplies the search view"))
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    #[::core::prelude::v1::test]
    fn searches_connected_clusters_and_keeps_favorites_after_disconnection() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let object = |namespace: &str, name: &str| json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":namespace,"labels":{"app":"api"}},"data":{"password":"never-search-this"}});
        let (first, a) = support::fixture(cx, "alpha", vec![object("team-a", "settings")]);
        let (second, b) = support::fixture(
            cx,
            "beta",
            vec![
                object("team-b", "settings"),
                json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"worker","namespace":"team-b"}}),
            ],
        );
        second.deny("secrets");
        cx.update(|cx| {
            cx.global::<SharedConnections>()
                .0
                .clone()
                .update(cx, |connections, _| {
                    connections.sessions.insert(a.id().clone(), a.clone());
                    connections.sessions.insert(b.id().clone(), b.clone());
                });
        });
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| GlobalSearch::new(window, cx))
            })
            .unwrap()
        });
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| !view.loading && view.visible.len() == 3)
        });
        view.read_with(cx, |view, _| {
            assert!(
                view.entries
                    .iter()
                    .all(|entry| !entry.searchable.contains("never-search-this"))
            );
            assert!(
                view.errors
                    .iter()
                    .any(|error| error.contains("Secret") && error.contains("403"))
            );
        });
        cx.update_window(window, |_, window, cx| {
            view.read(cx)
                .search
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
            window.input("beta ConfigMap app=api", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| view.visible.len() == 1)
        });
        let clicked = Arc::new(std::sync::Mutex::new(None));
        let observed = clicked.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&view, move |_, event: &OpenRequested, _| {
                if let OpenRequested::Resource {
                    cluster,
                    kind,
                    target,
                } = event
                {
                    *observed.lock().unwrap() = Some((
                        cluster.to_string(),
                        kind.resource.kind.clone(),
                        target.clone(),
                    ));
                }
            })
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("global-favorite", 0usize), cx);
            window.click(("global-open", 0usize), cx);
        })
        .unwrap();
        assert_eq!(
            *clicked.lock().unwrap(),
            Some((
                "beta".into(),
                "ConfigMap".into(),
                ObjectRef::new(Some("team-b".into()), "settings")
            ))
        );
        let file = std::fs::read(directory.path().join("settings.json")).unwrap();
        let persisted: settings::Preferences = serde_json::from_slice(&file).unwrap();
        assert_eq!(persisted.favorites.resources.len(), 1);
        cx.update_window(window, |_, window, cx| window.click("favorites-page", cx))
            .unwrap();
        cx.update(|cx| {
            cx.global::<SharedConnections>()
                .0
                .clone()
                .update(cx, |connections, cx| connections.disconnect(b.id(), cx))
        });
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| !view.loading && view.visible.is_empty())
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .find(("remove-unavailable-favorite", 0usize))
                    .visible()
            );
            window.click(("remove-unavailable-favorite", 0usize), cx);
        })
        .unwrap();
        assert!(cx.read(|cx| {
            settings::store(cx)
                .read(cx)
                .preferences
                .favorites
                .resources
                .is_empty()
        }));
        assert!(
            first
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(request, _)| request.starts_with("GET"))
        );
        assert!(
            second
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(request, _)| request.starts_with("GET"))
        );
    }
    #[::core::prelude::v1::test]
    fn opening_without_connections_does_not_connect_and_favorites_keep_source_identity() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| GlobalSearch::new(window, cx))
            })
            .unwrap()
        });
        assert!(!view.read_with(cx, |view, _| view.loading));
        assert!(cx.read(|cx| {
            cx.global::<SharedConnections>()
                .0
                .read(cx)
                .sessions
                .is_empty()
        }));
        let a = ClusterId::from_source("a", 1, "dev");
        let b = ClusterId::from_source("b", 1, "dev");
        cx.update(|cx| {
            toggle_cluster(&a, cx).unwrap();
            toggle_cluster(&b, cx).unwrap();
        });
        assert_eq!(
            cx.read(|cx| settings::store(cx)
                .read(cx)
                .preferences
                .favorites
                .clusters
                .len()),
            2
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("global-empty").visible());
        })
        .unwrap();
    }
}
