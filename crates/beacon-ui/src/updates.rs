//! One update state for every workspace and settings window.
use crate::{bridge::Bridge, settings};
use beacon_kube::connection::Proxy;
use beacon_updater::{Client, Downloaded, Installation, PreparedInstall, Release};
use futures::StreamExt as _;
use gpui_kit::*;
use std::{path::PathBuf, sync::Arc, time::Duration};

pub(crate) enum Status {
    Idle,
    Checking,
    Current,
    Available(Release),
    Downloading {
        release: Release,
        received: u64,
        total: u64,
    },
    Ready(Arc<Downloaded>),
    Installing,
    Failed {
        message: String,
        release: Option<Release>,
    },
}
pub(crate) struct Updater {
    pub status: Status,
    pub installation: Installation,
    pub last_checked: Option<std::time::SystemTime>,
    pub receipt: Option<String>,
    cache: PathBuf,
    generation: u64,
    abort: Option<tokio::task::AbortHandle>,
    task: Option<Task<()>>,
    timer: Option<Task<()>>,
    _preferences: Subscription,
    _prepared: Option<PreparedInstall>,
}
struct SharedUpdater(Entity<Updater>);
impl Global for SharedUpdater {}
pub(crate) fn maybe_store(cx: &App) -> Option<Entity<Updater>> {
    cx.try_global::<SharedUpdater>().map(|g| g.0.clone())
}
pub(crate) fn store(cx: &App) -> Entity<Updater> {
    cx.global::<SharedUpdater>().0.clone()
}

pub fn init(cx: &mut App) {
    let preferences = settings::store(cx);
    let cache = preferences.read(cx).directory.join("updates");
    let mut configuration = (
        preferences.read(cx).preferences.updates.channel,
        preferences.read(cx).preferences.proxy.clone(),
    );
    let mut auto_download = preferences.read(cx).preferences.updates.auto_download;
    let updater = cx.new(|cx| {
        let subscription = cx.subscribe(
            &preferences,
            move |view: &mut Updater, _, _: &settings::Changed, cx| {
                let preferences = settings::store(cx).read(cx).preferences.clone();
                let download_enabled = preferences.updates.auto_download && !auto_download;
                auto_download = preferences.updates.auto_download;
                let next = (preferences.updates.channel, preferences.proxy);
                if next != configuration && !matches!(view.status, Status::Installing) {
                    configuration = next;
                    view.cancel(cx);
                    view.status = Status::Idle;
                    if preferences.updates.auto_check {
                        view.check(cx);
                    }
                } else if download_enabled && matches!(view.status, Status::Available(_)) {
                    view.download(cx);
                }
            },
        );
        Updater {
            status: Status::Idle,
            installation: Installation::detect(),
            cache: cache.clone(),
            generation: 0,
            last_checked: None,
            receipt: None,
            abort: None,
            task: None,
            timer: None,
            _preferences: subscription,
            _prepared: None,
        }
    });
    cx.set_global(SharedUpdater(updater.clone()));
    updater.update(cx, |view, cx| {
        let reading = Bridge::global(cx).run(async move {
            tokio::task::spawn_blocking(move || beacon_updater::read_install_result(&cache))
                .await
                .ok()
                .flatten()
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Some(message)) = reading.await {
                let _ = this.update(cx, |view, cx| {
                    tracing::info!(%message, "update installation result");
                    view.receipt = Some(message);
                    cx.notify();
                });
            }
        })
        .detach();
        view.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(15))
                .await;
            loop {
                if this
                    .update(cx, |view, cx| {
                        if settings::store(cx).read(cx).preferences.updates.auto_check
                            && matches!(
                                view.status,
                                Status::Idle | Status::Current | Status::Failed { .. }
                            )
                        {
                            view.check(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(24 * 60 * 60))
                    .await;
            }
        }));
    });
}
impl Updater {
    fn client(cx: &App) -> anyhow::Result<Client> {
        let proxy = match &settings::store(cx).read(cx).preferences.proxy {
            Proxy::System => beacon_updater::Proxy::System,
            Proxy::Direct => beacon_updater::Proxy::Direct,
            Proxy::Custom(url) => beacon_updater::Proxy::Custom(url.clone()),
        };
        Client::new(proxy, beacon_updater::PUBLIC_KEY)
    }
    fn error(
        &mut self,
        error: impl std::fmt::Display,
        release: Option<Release>,
        cx: &mut Context<Self>,
    ) {
        let message = error.to_string();
        tracing::warn!(%message, "application update failed");
        self.status = Status::Failed { message, release };
        self.abort = None;
        cx.notify();
    }
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Installing) {
            return;
        }
        self.generation += 1;
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
        self.task = None;
        self.status = match &self.status {
            Status::Downloading { release, .. } => Status::Available(release.clone()),
            _ => Status::Idle,
        };
        cx.notify();
    }
    pub fn check(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.status,
            Status::Checking | Status::Downloading { .. } | Status::Installing | Status::Ready(_)
        ) {
            return;
        }
        let client = match Self::client(cx) {
            Ok(c) => c,
            Err(e) => {
                self.error(e, None, cx);
                return;
            }
        };
        self.cancel(cx);
        let generation = self.generation;
        self.status = Status::Checking;
        cx.notify();
        let installation = self.installation.clone();
        let channel = settings::store(cx).read(cx).preferences.updates.channel;
        let checking = Bridge::global(cx).run(async move {
            client
                .check(env!("CARGO_PKG_VERSION"), channel, &installation)
                .await
        });
        self.abort = Some(checking.abort_handle());
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = checking.await;
            let _ = this.update(cx, |view, cx| {
                if view.generation != generation {
                    return;
                }
                view.abort = None;
                view.last_checked = Some(std::time::SystemTime::now());
                match result {
                    Ok(Ok(Some(release))) => {
                        tracing::info!(version = %release.version, "application update available");
                        view.status = Status::Available(release);
                        if settings::store(cx)
                            .read(cx)
                            .preferences
                            .updates
                            .auto_download
                            && view.installation.can_install()
                        {
                            view.download(cx);
                        }
                    }
                    Ok(Ok(None)) => view.status = Status::Current,
                    Ok(Err(e)) => view.error(format!("{e:#}"), None, cx),
                    Err(e) => view.error(e, None, cx),
                }
                cx.notify();
            });
        }));
    }
    pub fn download(&mut self, cx: &mut Context<Self>) {
        let release = match &self.status {
            Status::Available(r)
            | Status::Failed {
                release: Some(r), ..
            } => r.clone(),
            _ => return,
        };
        if !self.installation.can_install() {
            return;
        }
        let client = match Self::client(cx) {
            Ok(c) => c,
            Err(e) => {
                self.error(e, Some(release), cx);
                return;
            }
        };
        self.cancel(cx);
        let generation = self.generation;
        let cache = self.cache.clone();
        self.status = Status::Downloading {
            release: release.clone(),
            received: 0,
            total: release.asset.size,
        };
        cx.notify();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let downloading = Bridge::global(cx).run(async move {
            let result = client
                .download(release, cache, |received, total| {
                    let _ = tx.unbounded_send(Progress::Bytes(received, total));
                })
                .await;
            let _ = tx.unbounded_send(Progress::Done(
                result.map(Arc::new).map_err(|e| format!("{e:#}")),
            ));
        });
        self.abort = Some(downloading.abort_handle());
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(progress) = rx.next().await {
                let done = matches!(progress, Progress::Done(_));
                if this
                    .update(cx, |view, cx| {
                        if view.generation != generation {
                            return;
                        }
                        match progress {
                            Progress::Bytes(n, size) => {
                                if let Status::Downloading {
                                    received, total, ..
                                } = &mut view.status
                                {
                                    *received = n;
                                    *total = size;
                                }
                            }
                            Progress::Done(Ok(download)) => {
                                view.abort = None;
                                view.status = Status::Ready(download);
                            }
                            Progress::Done(Err(error)) => {
                                let release =
                                    if let Status::Downloading { release, .. } = &view.status {
                                        Some(release.clone())
                                    } else {
                                        None
                                    };
                                view.error(error, release, cx);
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                    || done
                {
                    break;
                }
            }
        }));
    }
    pub fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Status::Ready(download) = &self.status else {
            return;
        };
        let download = download.clone();
        let blockers = crate::app::update_blockers(cx);
        if !blockers.edits.is_empty() {
            let detail = format!(
                "Apply or discard these edits before installing:\n\n{}",
                blockers.edits.join("\n")
            );
            let answer = window.prompt(
                PromptLevel::Warning,
                "Unapplied resource edits",
                Some(&detail),
                &["Cancel", "Return to editors"],
                cx,
            );
            cx.spawn(async move |_, cx| {
                if answer.await.ok() == Some(1) {
                    cx.update(crate::app::return_to_editors);
                }
            })
            .detach();
            return;
        }
        let detail = format!(
            "Install Beacon {} and restart?\n\n{} active shell/command session(s) and {} port forward(s) will stop. Clusters will stay disconnected after restart.",
            download.release.version, blockers.shells, blockers.forwards
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            "Restart and install update?",
            Some(&detail),
            &["Cancel", "Restart and install"],
            cx,
        );
        let installation = self.installation.clone();
        cx.spawn(async move |this, cx| {
            if answer.await.ok() != Some(1) {
                return;
            }
            let allowed = this.update(cx, |view, cx| {
                let same_package = matches!(&view.status, Status::Ready(ready) if Arc::ptr_eq(ready, &download));
                if !crate::app::update_blockers(cx).edits.is_empty() || !same_package {
                    return false;
                }
                view.status = Status::Installing;
                cx.notify();
                true
            }).unwrap_or(false);
            if !allowed {
                return;
            }
            let preparing = cx.update(|cx| {
                Bridge::global(cx).run(async move {
                    tokio::task::spawn_blocking(move || {
                        beacon_updater::prepare_install(download.clone(), installation)
                            .map_err(|error| (format!("{error:#}"), download))
                    }).await
                })
            });
            let result = preparing.await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(Ok(Ok(guard))) => {
                        // Edits may have appeared while the helper was copied.
                        // Launch and quit happen in one foreground callback.
                        if !crate::app::update_blockers(cx).edits.is_empty() {
                            view.status = Status::Ready(guard.download());
                            view.receipt = Some("New edits appeared while preparing the update. Apply or discard them before retrying.".into());
                            cx.notify();
                        } else if let Err(error) = guard.launch() {
                            tracing::warn!(%error, "could not launch update helper");
                            view.status = Status::Ready(guard.download());
                            view.receipt = Some(format!("{error:#}"));
                            cx.notify();
                        } else {
                            tracing::info!("restarting for application update");
                            view._prepared = Some(guard);
                            cx.quit();
                        }
                    }
                    Ok(Ok(Err((error, download)))) => {
                        tracing::warn!(%error, "could not prepare application update");
                        view.status = Status::Ready(download);
                        view.receipt = Some(error);
                        cx.notify();
                    }
                    Ok(Err(error)) => view.error(error, None, cx),
                    Err(error) => view.error(error, None, cx),
                }
            });
        }).detach();
    }
}
enum Progress {
    Bytes(u64, u64),
    Done(Result<Arc<Downloaded>, String>),
}
