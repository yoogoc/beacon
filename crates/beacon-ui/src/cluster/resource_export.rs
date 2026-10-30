use super::*;

impl ClusterView {
    pub(super) fn export_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.resource_exporting {
            return;
        }
        let Some(kind) = self
            .kind
            .as_ref()
            .filter(|kind| kind.supports("get"))
            .cloned()
        else {
            return;
        };
        // Freeze the selection and connection before the native save dialog.
        let targets = self.table.read(cx).delegate().selected_targets();
        let Some(name) = beacon_kube::export::suggested_filename(&kind, &targets) else {
            return;
        };
        let count = targets.len();
        let directory = directories::UserDirs::new()
            .map(|dirs| dirs.download_dir().unwrap_or(dirs.home_dir()).to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let prompt = cx.prompt_for_new_path(&directory, Some(&name));
        let session = self.session.clone();
        let bridge = Bridge::global(cx).clone();
        self.resource_exporting = true;
        self.outcome = Some(Outcome::Running(
            crate::i18n::text("Exporting resources").to_string(),
        ));
        self._resource_export = Some(cx.spawn_in(window, async move |this, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                other => {
                    let _ = this.update(cx, |view, cx| {
                        view.resource_exporting = false;
                        view.outcome = match other {
                            Ok(Ok(None)) => None,
                            _ => Some(Outcome::Failed(
                                crate::i18n::text(
                                    "Could not open the resource export save dialog.",
                                )
                                .to_string(),
                            )),
                        };
                        cx.notify();
                    });
                    return;
                }
            };
            if this.update(cx, |_, _| ()).is_err() {
                return;
            }
            let saved_path = path.display().to_string();
            let exporting = bridge.run(async move {
                beacon_kube::export::save(session, (*kind).clone(), targets, path).await
            });
            let result = exporting.await;
            let _ = this.update(cx, |view, cx| {
                view.resource_exporting = false;
                view.outcome = Some(match result {
                    Ok(Ok(())) => Outcome::Done(
                        crate::i18n::message(
                            "export.saved",
                            &[("count", count.to_string()), ("path", saved_path)],
                        )
                        .to_string(),
                    ),
                    Ok(Err(error)) => Outcome::Failed(export_error(error)),
                    Err(error) => Outcome::Failed(
                        crate::i18n::message("export.failed", &[("error", error.to_string())])
                            .to_string(),
                    ),
                });
                cx.notify();
            });
        }));
        cx.notify();
    }
}
fn export_error(error: beacon_kube::export::Failure) -> String {
    use beacon_kube::export::Failure;
    match error {
        Failure::Empty => crate::i18n::text("Select at least one resource to export.").to_string(),
        Failure::Resource { target, message } => crate::i18n::message(
            "export.resource_failed",
            &[
                ("target", target.to_string()),
                ("error", crate::i18n::text(message).to_string()),
            ],
        )
        .to_string(),
        Failure::File(error) => {
            crate::i18n::message("export.failed", &[("error", error)]).to_string()
        }
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests;
