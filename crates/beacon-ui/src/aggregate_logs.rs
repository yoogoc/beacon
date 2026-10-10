//! Live logs for every container in a selector, including new replicas.
use crate::{
    bridge::{Bridge, drain_into},
    copyable_text::copyable_text,
    pod_tools::{LogCanvas, LogLineWidths},
};
use beacon_kube::{
    ClusterSession, Delta, LogBuffer, LogEvent, LogOptions, ResourceStore, WatchKey,
    aggregate_logs::Source,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::Arc};

const ALL: &str = "All containers";
struct Running {
    restart: i64,
    _task: Task<()>,
}
pub(crate) struct AggregateLogs {
    session: Arc<ClusterSession>,
    namespaces: Vec<Option<String>>,
    selector: Entity<InputState>,
    search: Entity<InputState>,
    container: Entity<SelectState<SearchableVec<String>>>,
    container_names: Vec<String>,
    selected_container: Option<String>,
    pods: Vec<ResourceStore>,
    streams: BTreeMap<Source, Running>,
    errors: BTreeMap<Source, String>,
    logs: LogBuffer,
    display: Rc<RefCell<crate::pod_logs::Display>>,
    scroll: UniformListScrollHandle,
    wrapped: ListState,
    widths: LogLineWidths,
    wrap: bool,
    follow: bool,
    previous: bool,
    loading: usize,
    message: Option<String>,
    downloading: bool,
    _lists: Vec<Task<()>>,
    _watches: Vec<Task<()>>,
    _download: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

pub(crate) fn open(
    session: Arc<ClusterSession>,
    namespaces: Vec<Option<String>>,
    selector: String,
    window: &mut Window,
    cx: &mut App,
) -> Entity<AggregateLogs> {
    let title = format!("Aggregated logs · {}", session.id().display_name());
    let view = cx.new(|cx: &mut Context<AggregateLogs>| {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search all logs"));
        let container = cx.new(|cx| {
            SelectState::new(SearchableVec::new(vec![ALL.to_owned()]), None, window, cx)
                .searchable(true)
        });
        container.update(cx, |state, cx| {
            state.set_selected_value(&ALL.to_owned(), window, cx)
        });
        let mut view = AggregateLogs {
            session,
            namespaces,
            selector: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(selector)
                    .placeholder("Pod label selector")
            }),
            search: search.clone(),
            container: container.clone(),
            container_names: vec![ALL.into()],
            selected_container: None,
            pods: vec![],
            streams: BTreeMap::new(),
            errors: BTreeMap::new(),
            logs: LogBuffer::new(),
            display: Rc::default(),
            scroll: UniformListScrollHandle::new(),
            wrapped: ListState::new(0, ListAlignment::Top, px(300.)),
            widths: LogLineWidths::default(),
            wrap: false,
            follow: true,
            previous: false,
            loading: 0,
            message: None,
            downloading: false,
            _lists: vec![],
            _watches: vec![],
            _download: None,
            _subscriptions: vec![],
        };
        view._subscriptions
            .push(cx.subscribe(&search, |view, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view.follow = false;
                    view.update_display(cx);
                    cx.notify();
                }
            }));
        view._subscriptions.push(cx.subscribe_in(
            &container,
            window,
            |view, _, event: &SelectEvent<SearchableVec<String>>, window, cx| {
                if let SelectEvent::Confirm(Some(name)) = event {
                    view.selected_container = (name != ALL).then(|| name.clone());
                    view.clear_logs();
                    view.streams.clear();
                    view.sync(window, cx);
                }
            },
        ));
        view.load(window, cx);
        view
    });
    let content = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(title.clone())
            .width(px(1400.))
            .margin_top(px(24.))
            .child(content.clone())
    });
    view
}

impl AggregateLogs {
    fn clear_logs(&mut self) {
        self.logs.clear();
        *self.display.borrow_mut() = Default::default();
        self.widths = LogLineWidths::default();
        self.scroll = UniformListScrollHandle::new();
        self.wrapped.reset(0);
        self.errors.clear();
    }
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = match beacon_kube::labels::LabelSelector::parse(
            self.selector.read(cx).value().as_ref(),
        ) {
            Ok(selector) if !selector.is_empty() => selector.to_string(),
            Ok(_) => {
                self.message = Some(
                    "Enter a label selector to choose the Pods whose logs should be combined."
                        .into(),
                );
                cx.notify();
                return;
            }
            Err(error) => {
                self.message = Some(error);
                cx.notify();
                return;
            }
        };
        self._lists.clear();
        self._watches.clear();
        self.streams.clear();
        self.clear_logs();
        self.message = None;
        self.pods = self
            .namespaces
            .iter()
            .map(|_| ResourceStore::new())
            .collect();
        self.loading = self.namespaces.len();
        for (index, namespace) in self.namespaces.clone().into_iter().enumerate() {
            let mut key = WatchKey::all(beacon_kube::resources::pod()).in_namespace(namespace);
            key.labels = Some(query.clone());
            let session = self.session.clone();
            let requested = key.clone();
            let listing = Bridge::global(cx).run_cancellable(async move {
                session
                    .list_objects(requested)
                    .await
                    .map_err(|error| error.user_message())
            });
            self._lists.push(cx.spawn_in(window, async move |this, cx| {
                let result = listing.result().await;
                let _ = this.update_in(cx, |view, window, cx| {
                    view.loading = view.loading.saturating_sub(1);
                    match result {
                        Ok(Ok(objects)) => {
                            view.pods[index].apply(Delta::Reset(objects));
                            view.sync(window, cx);
                            view._watches.push(drain_into(
                                cx,
                                view.session.subscribe(key),
                                move |view, batch, window, cx| {
                                    if view.pods[index].apply_batch(batch) {
                                        view.sync(window, cx);
                                    }
                                },
                                window,
                            ));
                        }
                        Ok(Err(error)) => view.message = Some(error),
                        Err(error) => view.message = Some(error.to_string()),
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let all = beacon_kube::aggregate_logs::sources(
            self.pods
                .iter()
                .flat_map(|store| store.iter().map(|(_, pod)| pod)),
            None,
        );
        let mut names = vec![ALL.to_owned()];
        names.extend(all.keys().map(|source| source.container.clone()));
        names[1..].sort();
        names.dedup();
        if names != self.container_names {
            self.container_names = names.clone();
            let selected = self
                .selected_container
                .clone()
                .unwrap_or_else(|| ALL.into());
            self.container.update(cx, |state, cx| {
                state.set_items(SearchableVec::new(names), window, cx);
                state.set_selected_value(&selected, window, cx);
            });
        }
        let desired = beacon_kube::aggregate_logs::sources(
            self.pods
                .iter()
                .flat_map(|store| store.iter().map(|(_, pod)| pod)),
            self.selected_container.as_deref(),
        );
        self.streams
            .retain(|source, running| desired.get(source) == Some(&running.restart));
        self.errors.retain(|source, _| desired.contains_key(source));
        for (source, restart) in desired {
            if self.streams.contains_key(&source) {
                continue;
            }
            let stream = self.session.follow_logs(
                source.namespace.clone(),
                source.pod.clone(),
                LogOptions {
                    container: Some(source.container.clone()),
                    timestamps: true,
                    previous: self.previous,
                },
            );
            let identity = source.clone();
            let task = drain_into(
                cx,
                stream,
                move |view, event, _, cx| {
                    match event {
                        LogEvent::Lines(lines) => {
                            let tail = view.follow && view.at_tail();
                            view.logs.extend(
                                lines
                                    .into_iter()
                                    .map(|line| format!("{} {line}", identity.prefix())),
                            );
                            view.update_display(cx);
                            if tail {
                                view.latest();
                            }
                            view.errors.remove(&identity);
                        }
                        LogEvent::Failed(error) => {
                            view.errors.insert(identity.clone(), error);
                        }
                        LogEvent::Closed => {}
                    }
                    cx.notify();
                },
                window,
            );
            self.streams.insert(
                source,
                Running {
                    restart,
                    _task: task,
                },
            );
        }
        cx.notify();
    }
    fn at_tail(&self) -> bool {
        if self.wrap {
            return self.wrapped.is_following_tail();
        }
        let handle = self.scroll.0.borrow();
        let offset = handle.base_handle.offset().y;
        let maximum = handle.base_handle.max_offset().y;
        maximum + offset <= px(24.)
    }
    fn latest(&self) {
        let count = self.display.borrow().rows.len();
        if count > 0 {
            self.scroll
                .scroll_to_item(count - 1, ScrollStrategy::Bottom);
            self.wrapped.scroll_to_end();
        }
    }
    fn update_display(&mut self, cx: &App) {
        let change = self
            .display
            .borrow_mut()
            .update(&self.logs, &self.search.read(cx).value());
        if change.reset {
            self.scroll = UniformListScrollHandle::new();
            self.wrapped.reset(self.display.borrow().rows.len());
        } else {
            if change.removed > 0 {
                self.wrapped.splice(0..change.removed, 0);
            }
            if change.added > 0 {
                let count = self.wrapped.item_count();
                self.wrapped.splice(count..count, change.added);
            }
        }
    }
    fn download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.downloading {
            return;
        }
        let sources: Vec<_> = self.streams.keys().cloned().collect();
        if sources.is_empty() {
            return;
        }
        let choosing = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a folder for aggregated logs".into()),
        });
        let bridge = Bridge::global(cx).clone();
        let session = self.session.clone();
        let previous = self.previous;
        self.downloading = true;
        self.message = Some("Choose a download folder…".into());
        self._download = Some(cx.spawn_in(window, async move |this, cx| {
            let folder = match choosing.await {
                Ok(Ok(Some(paths))) if !paths.is_empty() => paths[0].clone(),
                Ok(Ok(_)) => {
                    let _ = this.update(cx, |view, cx| {
                        view.downloading = false;
                        view.message = Some("Download cancelled.".into());
                        cx.notify();
                    });
                    return;
                }
                other => {
                    let _ = this.update(cx, |view, cx| {
                        view.downloading = false;
                        view.message = Some(format!("Could not choose a folder: {other:?}"));
                        cx.notify();
                    });
                    return;
                }
            };
            let directory = folder.join(format!(
                "Beacon-logs-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            ));
            let destination = directory.clone();
            let _ = this.update(cx, |view, cx| {
                view.message = Some("Downloading complete available logs for each source…".into());
                cx.notify();
            });
            let downloading = bridge.run_cancellable(async move {
                tokio::fs::create_dir(&destination)
                    .await
                    .map_err(|error| error.to_string())?;
                let mut failures = Vec::new();
                let mut count = 0;
                for source in sources {
                    let path = destination.join(format!(
                        "{}_{}_{}_{}.log",
                        source.namespace, source.pod, source.container, source.uid
                    ));
                    let options = LogOptions {
                        container: Some(source.container.clone()),
                        previous,
                        timestamps: true,
                    };
                    match session
                        .clone()
                        .download_logs(source.namespace.clone(), source.pod.clone(), options, path)
                        .await
                    {
                        Ok(_) => count += 1,
                        Err(error) => failures.push(format!("{} {error}", source.prefix())),
                    }
                }
                Ok::<_, String>((count, failures))
            });
            let result = downloading.result().await;
            let _ = this.update(cx, |view, cx| {
                view.downloading = false;
                view.message = Some(match result {
                    Ok(Ok((count, failures))) => format!(
                        "Saved {count} files to {}{}",
                        directory.display(),
                        if failures.is_empty() {
                            String::new()
                        } else {
                            format!("\n{}", failures.join("\n"))
                        }
                    ),
                    Ok(Err(error)) => error,
                    Err(error) => error.to_string(),
                });
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for AggregateLogs {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = LogCanvas {
            logs: &self.logs,
            display: self.display.clone(),
            wrap: self.wrap,
            wrapped: &self.wrapped,
            unwrapped: &self.scroll,
            widths: &mut self.widths,
        }
        .render(window, cx);
        v_flex().w_full().h((window.viewport_size().height * 0.78).min(px(760.))).gap_2()
            .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.selector).small()))
                .child(Button::new("apply-log-selector").small().ghost().label("Apply selector / retry").on_click(cx.listener(|view, _, window, cx| view.load(window, cx)))))
            .child(h_flex().id("aggregate-log-toolbar").w_full().overflow_x_scroll().gap_2().flex_shrink_0()
                .child(div().w(px(210.)).flex_shrink_0().child(Select::new(&self.container).small().title_prefix("Container: ")))
                .child(div().w(px(180.)).flex_shrink_0().child(Input::new(&self.search).small()))
                .child(Button::new("aggregate-pause").small().ghost().label(if self.follow { "Pause follow" } else { "Resume follow" }).on_click(cx.listener(|view, _, _, cx| { view.follow = !view.follow; if view.follow { view.latest(); } cx.notify(); })))
                .child(Button::new("aggregate-latest").small().ghost().label("Latest").on_click(cx.listener(|view, _, _, cx| { view.follow = true; view.latest(); cx.notify(); })))
                .child(Button::new("aggregate-wrap").small().ghost().label("Wrap").on_click(cx.listener(|view, _, _, cx| { view.wrap = !view.wrap; if view.follow { view.latest(); } cx.notify(); })))
                .child(Button::new("aggregate-previous").small().ghost().label(if self.previous { "Previous: on" } else { "Previous" }).on_click(cx.listener(|view, _, window, cx| { view.previous = !view.previous; view.clear_logs(); view.streams.clear(); view.sync(window, cx); })))
                .child(Button::new("aggregate-copy").small().ghost().label("Copy").on_click(cx.listener(|view, _, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(view.display.borrow().text())))))
                .child(Button::new("aggregate-download").small().ghost().label("Download…").disabled(self.downloading || self.streams.is_empty()).on_click(cx.listener(|view, _, window, cx| view.download(window, cx))))
                .when(self.downloading, |bar| bar.child(Button::new("aggregate-cancel-download").small().ghost().label("Cancel download").on_click(cx.listener(|view, _, _, cx| { view._download = None; view.downloading = false; view.message = Some("Download cancelled. Completed files remain in the selected folder.".into()); cx.notify(); }))))
                .child(div().whitespace_nowrap().text_sm().child(format!("{} Pods · {} sources · {} lines · {} dropped", self.pods.iter().map(ResourceStore::len).sum::<usize>(), self.streams.len(), self.display.borrow().rows.len(), self.logs.dropped()))))
            .children(self.message.as_ref().map(|message| copyable_text("aggregate-message", message.clone())))
            .when(self.loading > 0, |view| view.child("Loading matching Pods…"))
            .children(self.errors.iter().enumerate().map(|(index, (source, error))| copyable_text(("aggregate-stream-error", index), format!("{} {error}", source.prefix()))))
            .child(div().flex_1().min_size_0().child(body))
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    #[::core::prelude::v1::test]
    fn matching_new_pods_and_restarted_containers_join_live_logs() {
        let cx = &mut support::context();
        let pod = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web-1","namespace":"default","uid":"one","resourceVersion":"1","labels":{"app":"web"}},"spec":{"containers":[{"name":"app","image":"busybox"}]},"status":{"phase":"Running","containerStatuses":[{"name":"app","restartCount":0,"state":{"running":{}}}]}});
        let (fixture, session) = support::fixture(cx, "aggregate-fixture", vec![pod.clone()]);
        let window = support::window(cx);
        let view = cx
            .update_window(window, |_, window, cx| {
                open(
                    session,
                    vec![Some("default".into())],
                    "app=web".into(),
                    window,
                    cx,
                )
            })
            .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| view.logs.len() == 1) && fixture.watchers() > 0
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("aggregate-download").visible());
            window.click("aggregate-pause", cx);
        })
        .unwrap();
        assert!(!view.read_with(cx, |view, _| view.follow));
        let mut next = pod.clone();
        next["metadata"]["name"] = json!("web-2");
        next["metadata"]["uid"] = json!("two");
        fixture.put(next);
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.streams.len() == 2 && view.logs.len() == 2
            })
        });
        let mut restart = pod;
        restart["metadata"]["resourceVersion"] = json!("2");
        restart["status"]["containerStatuses"][0]["restartCount"] = json!(1);
        fixture.put(restart);
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.logs.len() == 3));
        let logs = view.read_with(cx, |view, _| view.logs.to_text());
        assert!(logs.contains("[default/web-1:app]"));
        assert!(logs.contains("[default/web-2:app]"));
        cx.update_window(window, |_, window, cx| {
            window.click("aggregate-wrap", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert!(view.read_with(cx, |view, _| view.wrap));
        // The downloaded snapshot includes all sources, independent of search.
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.search
                    .update(cx, |input, cx| input.set_value("no-match", window, cx))
            });
            window.render_frame(cx);
            window.click("aggregate-download", cx);
        })
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().to_owned();
        cx.simulate_path_prompt_response(move |options| {
            assert!(options.directories && !options.files && !options.multiple);
            Some(vec![destination])
        });
        support::settle(cx, |cx| view.read_with(cx, |view, _| !view.downloading));
        let output = std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let files = std::fs::read_dir(output)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 2);
        for file in files {
            assert!(
                std::fs::read_to_string(file)
                    .unwrap()
                    .contains("fixture log")
            );
        }
    }
}
