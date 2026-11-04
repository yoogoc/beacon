use super::*;
use beacon_kube::sources::export::{self, Failure};
use gpui_kit::component::notification::Notification;

impl BeaconApp {
    pub(super) fn export_cluster(
        &mut self,
        id: ClusterId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.cluster_exports.contains(&id) {
            return;
        }
        // Freeze the exact source/context before showing the native save dialog.
        // Same-named contexts in different sources must never fall back to the
        // system kubeconfig or to whichever cluster becomes active meanwhile.
        let target = crate::sources::store(cx)
            .read(cx)
            .catalog
            .targets
            .get(&id)
            .cloned();
        let Some(target) = target else {
            window.push_notification(Notification::error(crate::i18n::text(
                "This cluster's kubeconfig source is unavailable. Reload its source before exporting.",
            )), cx);
            return;
        };
        let name = export::filename(self.base_cluster_name(&id));
        let label = self.cluster_display_name(&id);
        let directory = directories::UserDirs::new()
            .map(|dirs| dirs.download_dir().unwrap_or(dirs.home_dir()).to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let choosing = cx.prompt_for_new_path(&directory, Some(&name));
        let executor = cx.background_executor().clone();
        self.cluster_exports.insert(id.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let path = match choosing.await {
                Ok(Ok(Some(path))) => path,
                other => {
                    let _ = this.update_in(cx, |view, window, cx| {
                        view.cluster_exports.remove(&id);
                        if !matches!(other, Ok(Ok(None))) {
                            window.push_notification(
                                Notification::error(crate::i18n::text(
                                    "Could not open the cluster export save dialog.",
                                )),
                                cx,
                            );
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            if this.update(cx, |_, _| ()).is_err() {
                return;
            }
            let saved_path = path.display().to_string();
            let result = executor
                .spawn(async move { export::save(&target, &path) })
                .await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.cluster_exports.remove(&id);
                let notification = match result {
                    Ok(()) => Notification::success(crate::i18n::message(
                        "cluster_export.saved",
                        &[("cluster", label), ("path", saved_path)],
                    )),
                    Err(error) => Notification::error(crate::i18n::message(
                        "cluster_export.failed",
                        &[("error", export_error(error))],
                    )),
                };
                window.push_notification(notification, cx);
                cx.notify();
            });
        })
        .detach();
    }
}

fn export_error(error: Failure) -> String {
    match error {
        Failure::Reference { field, reason } => crate::i18n::message(
            "cluster_export.read_reference",
            &[("field", field.into()), ("error", reason)],
        )
        .to_string(),
        Failure::TooLarge(field) => crate::i18n::message(
            "cluster_export.reference_too_large",
            &[("field", field.into())],
        )
        .to_string(),
        Failure::File(reason) => reason,
        error => crate::i18n::text(error.to_string()).to_string(),
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests;
