//! Shared source catalog. Files are parsed off the foreground thread and all
//! workspaces observe the same generation. Reloading never reconnects sessions.
mod editor;
mod view;
use beacon_kube::sources::{self, Catalog};
use gpui_kit::*;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
pub(crate) use view::SourcesView;

pub(crate) struct Changed;
pub(crate) struct State {
    pub catalog: Arc<Catalog>,
    pub labels: std::collections::HashMap<beacon_kube::ClusterId, String>,
    pub loading: bool,
    pub loaded: Option<Instant>,
    system: Vec<PathBuf>,
    revision: u64,
    _load: Option<Task<()>>,
    _watch: Option<Task<()>>,
    _settings: Option<Subscription>,
}
impl EventEmitter<Changed> for State {}
struct Store(Entity<State>);
impl Global for Store {}
pub(crate) fn store(cx: &App) -> Entity<State> {
    cx.global::<Store>().0.clone()
}
pub(crate) fn init(cx: &mut App) {
    let state = cx.new(|_| State {
        catalog: Arc::default(),
        labels: Default::default(),
        loading: false,
        loaded: None,
        system: if cfg!(test) {
            vec![]
        } else {
            sources::system_paths()
        },
        revision: 0,
        _load: None,
        _watch: None,
        _settings: None,
    });
    cx.set_global(Store(state.clone()));
    let settings = crate::settings::store(cx);
    state.update(cx, |state, cx| {
        state._settings = Some(cx.subscribe(&settings, |state, _, _, cx| state.reload(true, cx)));
        if cfg!(test) {
            state.loaded = Some(Instant::now());
        } else {
            state.reload(true, cx);
        }
        // Test contexts control reloads explicitly; no host files or endless
        // deterministic timer pumps enter UI integration fixtures.
        if !cfg!(test) {
            state._watch = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    if this
                        .update(cx, |state, cx| {
                            if !state.loading {
                                state.reload(false, cx);
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
    });
}
impl State {
    pub fn reload(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.revision += 1;
        let revision = self.revision;
        self.loading = true;
        let settings = crate::settings::store(cx).read(cx);
        let preferences = settings.preferences.kubeconfigs.clone();
        let directory = settings.directory.clone();
        let system = self.system.clone();
        let loading = cx
            .background_executor()
            .spawn(async move { sources::load(&preferences, &directory, &system) });
        self._load = Some(cx.spawn(async move |this, cx| {
            let catalog = loading.await;
            let _ = this.update(cx, |state, cx| {
                if state.revision != revision {
                    return;
                }
                state.loading = false;
                let changed = state.catalog.fingerprint != catalog.fingerprint;
                if changed || visible || state.loaded.is_none() {
                    let mut labels = catalog.labels();
                    for (id, label) in &state.labels {
                        let connections = cx
                            .global::<crate::connections::SharedConnections>()
                            .0
                            .read(cx);
                        if connections.sessions.contains_key(id) || connections.connecting(id) {
                            labels.entry(id.clone()).or_insert_with(|| label.clone());
                        }
                    }
                    state.labels = labels;
                    state.catalog = Arc::new(catalog);
                    state.loaded = Some(Instant::now());
                    cx.emit(Changed);
                    cx.notify();
                }
            });
        }));
        if visible {
            cx.notify();
        }
    }
}
