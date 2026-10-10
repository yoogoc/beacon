//! An immutable manifest preview. Only an explicit confirmation emits a write.

use beacon_kube::ops;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use similar::TextDiff;

use crate::theme::{BeaconTheme as _, Tone};

pub(crate) struct Preview {
    manifest: Value,
    files: Vec<DiffFile>,
    changed: bool,
}

impl Preview {
    pub fn apply(current: Value, proposed: Value) -> Result<Self, String> {
        // A renamed manifest must not be reviewed as though it were the selected object.
        for field in ["/apiVersion", "/kind", "/metadata/name"] {
            if current.pointer(field) != proposed.pointer(field) {
                return Err(
                    "Keep the resource's apiVersion, kind and name unchanged when applying YAML."
                        .into(),
                );
            }
        }
        if let Some(namespace) = proposed
            .pointer("/metadata/namespace")
            .filter(|value| !value.is_null())
            && current.pointer("/metadata/namespace") != Some(namespace)
        {
            return Err("Keep the resource's namespace unchanged when applying YAML.".into());
        }
        let before = canonical_yaml(ops::prepare_apply(current))?;
        Self::new(&before, ops::prepare_apply(proposed))
    }

    pub fn text(before: &str, after: String) -> Result<Self, String> {
        let patch = TextDiff::from_lines(before, &after)
            .unified_diff()
            .context_radius(before.lines().count().max(after.lines().count()))
            .header("file", "file")
            .to_string();
        let changed = before != after;
        let files = if changed {
            DiffFile::parse(&patch).map_err(|e| e.to_string())?
        } else {
            vec![DiffFile::unchanged("file", &after)]
        };
        Ok(Self {
            manifest: Value::String(after),
            files,
            changed,
        })
    }

    pub fn create(proposed: Value) -> Result<Self, String> {
        Self::new("", proposed)
    }

    fn new(before: &str, manifest: Value) -> Result<Self, String> {
        let after = canonical_yaml(manifest.clone())?;
        let changed = before != after;
        let files = if changed {
            // Supply the whole document so the viewer can expand unchanged sections.
            let patch = TextDiff::from_lines(before, &after)
                .unified_diff()
                .context_radius(before.lines().count().max(after.lines().count()))
                .header(
                    if before.is_empty() {
                        "/dev/null"
                    } else {
                        "resource.yaml"
                    },
                    "resource.yaml",
                )
                .to_string();
            DiffFile::parse(&patch)
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|file| file.with_language("yaml"))
                .collect()
        } else {
            vec![DiffFile::unchanged("resource.yaml", &after).with_language("yaml")]
        };
        Ok(Self {
            manifest,
            files,
            changed,
        })
    }
}

pub(crate) fn canonical_yaml(mut value: Value) -> Result<String, String> {
    fn sort(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.values_mut().for_each(sort);
                map.sort_keys();
            }
            Value::Array(items) => items.iter_mut().for_each(sort),
            _ => {}
        }
    }
    sort(&mut value);
    serde_saphyr::to_string(&value).map_err(|error| error.to_string())
}

pub(crate) enum ReviewEvent {
    Confirmed(Box<Value>),
    Cancelled,
}

pub(crate) struct ReviewView {
    preview: Preview,
    diff: Entity<DiffState>,
    context: String,
    create: bool,
    force: bool,
    resolved: bool,
    confirmation_label: Option<String>,
    confirmation_allowed: bool,
}

impl EventEmitter<ReviewEvent> for ReviewView {}

pub(crate) fn open(
    preview: Preview,
    context: String,
    create: bool,
    force: bool,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ReviewView> {
    open_titled(
        preview,
        context,
        create,
        force,
        "Review YAML changes",
        window,
        cx,
    )
}

pub(crate) fn open_text(
    preview: Preview,
    context: String,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ReviewView> {
    let review = open_titled(
        preview,
        context,
        false,
        false,
        "Review file changes",
        window,
        cx,
    );
    review.update(cx, |view, _| view.confirmation_label("Confirm and save"));
    review
}

fn open_titled(
    preview: Preview,
    context: String,
    create: bool,
    force: bool,
    title: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ReviewView> {
    let review = cx.new(|cx| {
        let diff =
            cx.new(|cx| DiffState::new(preview.files.clone(), cx).with_mode(DiffMode::Split));
        ReviewView {
            preview,
            diff,
            context,
            create,
            force,
            resolved: false,
            confirmation_label: None,
            confirmation_allowed: true,
        }
    });
    let content = review.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let confirmed = content.clone();
        let closed = content.clone();
        dialog
            .title(title)
            .width(px(1200.))
            .margin_top(px(32.))
            .on_ok(move |_, _, cx| confirmed.update(cx, |view, cx| view.confirm(cx)))
            .on_close(move |_, _, cx| closed.update(cx, |view, cx| view.cancel(cx)))
            .child(content.clone())
    });
    review
}

impl ReviewView {
    pub(crate) fn confirmation_label(&mut self, label: &str) {
        self.confirmation_label = Some(label.into());
    }
    pub(crate) fn read_only(&mut self) {
        self.confirmation_allowed = false;
    }
    fn confirm(&mut self, cx: &mut Context<Self>) -> bool {
        if self.resolved || !self.preview.changed || !self.confirmation_allowed {
            return false;
        }
        self.resolved = true;
        cx.emit(ReviewEvent::Confirmed(Box::new(
            self.preview.manifest.clone(),
        )));
        true
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.resolved {
            self.resolved = true;
            cx.emit(ReviewEvent::Cancelled);
        }
    }
}

impl Render for ReviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.diff.read(cx).mode();
        let label = if let Some(label) = self.confirmation_label.as_deref() {
            label
        } else if self.create {
            "Confirm and create"
        } else if self.force {
            "Confirm force apply"
        } else {
            "Confirm and apply"
        };
        v_flex()
            .w_full()
            .h((window.viewport_size().height * 0.75).min(px(680.)))
            .gap_3()
            .child(crate::copyable_text::copyable_text(
                "yaml-review-context",
                self.context.clone(),
            ))
            .when(self.force, |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().tone(Tone::Warning))
                        .child("Force apply will take ownership of the conflicting fields."),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("diff-split")
                            .small()
                            .ghost()
                            .label("Side by side")
                            .selected(mode == DiffMode::Split)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.diff
                                    .update(cx, |state, cx| state.set_mode(DiffMode::Split, cx))
                            })),
                    )
                    .child(
                        Button::new("diff-unified")
                            .small()
                            .ghost()
                            .label("Unified")
                            .selected(mode == DiffMode::Unified)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.diff
                                    .update(cx, |state, cx| state.set_mode(DiffMode::Unified, cx))
                            })),
                    )
                    .child(
                        Button::new("diff-show-all")
                            .small()
                            .ghost()
                            .label("Show all lines")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.diff.update(cx, |state, cx| state.expand_unchanged(cx))
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().flex_1().child(if self.create {
                        "Current: new resource"
                    } else {
                        if self.preview.manifest.is_string() {
                            "− Current file"
                        } else {
                            "− Current cluster YAML"
                        }
                    }))
                    .child(div().flex_1().child(if self.preview.manifest.is_string() {
                        "+ Proposed file"
                    } else {
                        "+ Proposed YAML"
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(Diff::new(&self.diff).size_full()),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if self.preview.changed {
                                "Review the highlighted changes before saving."
                            } else {
                                "No changes to save."
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("cancel-yaml-review")
                                    .ghost()
                                    .label("Back to editing")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.cancel(cx);
                                        // Deliver the event while the dialog still owns its view.
                                        // Subscribers hold only a weak handle to the emitter.
                                        window.defer(cx, |window, cx| window.close_dialog(cx));
                                    })),
                            )
                            .child(
                                Button::new("confirm-yaml-review")
                                    .primary()
                                    .label(label)
                                    .disabled(
                                        !self.preview.changed
                                            || self.resolved
                                            || !self.confirmation_allowed,
                                    )
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        if view.confirm(cx) {
                                            window.defer(cx, |window, cx| window.close_dialog(cx));
                                        }
                                    })),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Preview, canonical_yaml};
    use serde_json::json;

    fn resource() -> serde_json::Value {
        json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"demo", "namespace":"default", "managedFields":[{"manager":"controller"}]}, "data":{"message":"before"}})
    }

    #[test]
    fn preview_uses_the_apply_payload_and_ignores_field_ownership_records() {
        let before = resource();
        let mut proposed = before.clone();
        proposed["metadata"]["managedFields"] = json!([]);
        let preview = Preview::apply(before.clone(), proposed).unwrap();
        assert!(!preview.changed);
        assert!(
            preview
                .manifest
                .pointer("/metadata/managedFields")
                .is_none()
        );
        assert!(before.pointer("/metadata/managedFields").is_some());
    }

    #[test]
    fn preview_includes_additions_deletions_and_the_frozen_manifest() {
        let before = resource();
        let mut proposed = before.clone();
        proposed["data"]["message"] = json!("after");
        let preview = Preview::apply(before, proposed.clone()).unwrap();
        proposed["data"]["message"] = json!("later edit");
        assert!(preview.changed);
        assert_eq!(preview.files[0].additions(), 1);
        assert_eq!(preview.files[0].deletions(), 1);
        assert_eq!(preview.manifest["data"]["message"], "after");
    }

    #[test]
    fn formatting_and_mapping_order_do_not_create_false_changes() {
        let one: serde_json::Value =
            serde_saphyr::from_str("data: {z: 2, a: 1}\nkind: ConfigMap\n").unwrap();
        let two: serde_json::Value =
            serde_saphyr::from_str("kind: ConfigMap\ndata:\n  a: 1\n  z: 2\n").unwrap();
        assert_eq!(canonical_yaml(one).unwrap(), canonical_yaml(two).unwrap());
    }

    #[test]
    fn new_resources_are_all_additions_and_apply_cannot_change_identity() {
        let mut proposed = resource();
        let preview = Preview::create(proposed.clone()).unwrap();
        assert!(preview.changed);
        assert_eq!(preview.files[0].deletions(), 0);
        assert!(preview.files[0].additions() > 0);
        for field in [
            "/metadata/name",
            "/metadata/namespace",
            "/kind",
            "/apiVersion",
        ] {
            *proposed.pointer_mut(field).unwrap() = json!("different");
            assert!(Preview::apply(resource(), proposed.clone()).is_err());
            proposed = resource();
        }
    }
}
