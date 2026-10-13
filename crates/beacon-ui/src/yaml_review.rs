//! An immutable manifest preview. Only an explicit confirmation emits a write.

mod presentation;

use beacon_kube::ops;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::diff::{DiffFile, DiffLinePosition, DiffMode, DiffSide, DiffState};
use gpui_kit::*;
use serde_json::Value;
use similar::TextDiff;

pub(crate) struct Preview {
    manifest: Value,
    files: Vec<DiffFile>,
    changed: bool,
    changes: Vec<DiffLinePosition>,
}

impl Preview {
    /// Read-only surfaces share the review's complete-context diff preparation.
    pub(crate) fn files(&self) -> &[DiffFile] {
        &self.files
    }

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
            changes: change_positions(before, &after, "file"),
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
            changes: change_positions(before, &after, "resource.yaml"),
            manifest,
            files,
            changed,
        })
    }
}

fn change_positions(before: &str, after: &str, path: &str) -> Vec<DiffLinePosition> {
    let mut positions = Vec::new();
    let mut changing = false;
    for change in TextDiff::from_lines(before, after).iter_all_changes() {
        let (side, index) = match change.tag() {
            similar::ChangeTag::Equal => {
                changing = false;
                continue;
            }
            similar::ChangeTag::Delete => (DiffSide::Original, change.old_index()),
            similar::ChangeTag::Insert => (DiffSide::Modified, change.new_index()),
        };
        if !changing && let Some(index) = index {
            positions.push(DiffLinePosition::new(path.to_owned(), side, index + 1));
        }
        changing = true;
    }
    positions
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
    title: &'static str,
    change: usize,
    show_context: bool,
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
        "Review changes",
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
    review.update(cx, |view, _| view.confirmation_label("Confirm & save"));
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
        let diff = cx.new(|cx| {
            DiffState::new(preview.files.clone(), cx)
                .with_mode(if preview.changed {
                    DiffMode::Split
                } else {
                    DiffMode::Unified
                })
                .with_context_lines(Some(2))
        });
        ReviewView {
            preview,
            diff,
            context,
            title,
            change: 0,
            show_context: false,
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
            .close_button(false)
            .p_0()
            .gap_0()
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

#[cfg(all(test, feature = "ui-tests"))]
mod rendering_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;

    #[::core::prelude::v1::test]
    fn review_navigation_context_and_confirmation_keep_the_frozen_draft() {
        use std::{cell::RefCell, rc::Rc};
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let window = support::window(cx);
        let before = (0..24)
            .map(|i| format!("key_{i}: old\n"))
            .collect::<String>();
        let after = before
            .replace("key_0: old", "key_0: new")
            .replace("key_20: old", "key_20: new");
        let confirmed = Rc::new(RefCell::new(Vec::new()));
        let (review, _subscription) = cx
            .update_window(window, |_, window, cx| {
                window.resize(size(px(700.), px(700.)));
                let review = open_text(
                    Preview::text(&before, after.clone()).unwrap(),
                    "dev · default · app · /etc/config.yaml".into(),
                    window,
                    cx,
                );
                let recorded = confirmed.clone();
                let subscription = cx.subscribe(&review, move |_, event: &ReviewEvent, _| {
                    if let ReviewEvent::Confirmed(value) = event {
                        recorded.borrow_mut().push((**value).clone());
                    }
                });
                (review, subscription)
            })
            .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click("diff-next", cx);
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(window.within(("new", 20usize)).find("source").visible());
            assert_eq!(review.read(cx).change, 1);
            window.click("diff-show-all", cx);
            assert_eq!(review.read(cx).diff.read(cx).context_lines(), None);
            window.click("diff-show-all", cx);
            assert_eq!(review.read(cx).diff.read(cx).context_lines(), Some(2));
            window.click("diff-previous", cx);
            assert_eq!(review.read(cx).change, 0);
            assert!(confirmed.borrow().is_empty());
            let button = window.find("confirm-yaml-review");
            assert!(button.visible());
            assert!(button.bounds().right() <= window.viewport_size().width);
            assert!(button.bounds().bottom() <= window.viewport_size().height);
            window.click("confirm-yaml-review", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(&*confirmed.borrow(), &[Value::String(after)]);
    }

    #[::core::prelude::v1::test]
    fn split_review_keeps_both_long_yaml_values_visible_and_copies_the_original_line() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let window = support::window(cx);
        let before = json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": "review", "namespace": "default"},
            "data": {"setting": "original-value-".repeat(40)},
        });
        let mut after = before.clone();
        after["data"]["setting"] = json!("modified-value-".repeat(40));
        let yaml = canonical_yaml(after.clone()).unwrap();
        let line = yaml
            .lines()
            .position(|line| line.contains("modified-value-"))
            .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.resize(size(px(1400.), px(1000.)));
            open(
                Preview::apply(before, after).unwrap(),
                "ConfigMap / default / review".into(),
                false,
                false,
                window,
                cx,
            );
        })
        .unwrap();
        for width in [1400., 1000., 700.] {
            cx.update_window(window, |_, window, cx| {
                window.resize(size(px(width), px(1000.)));
                window.render_frame(cx);
                window.render_frame(cx);
                let viewport = window.find("yaml-review-diff").bounds();
                let old = window.within(("old", line)).find("source");
                let new = window.within(("new", line)).find("source");
                assert!(old.visible(), "current YAML is hidden at {width}");
                assert!(new.visible(), "proposed YAML is hidden at {width}");
                assert!(old.bounds().right() <= viewport.center().x + px(1.));
                assert!(new.bounds().left() >= viewport.center().x);
                assert!(
                    new.bounds().right() <= viewport.right(),
                    "proposed YAML extends outside the viewer at {width}: {:?} / {viewport:?}",
                    new.bounds()
                );
                assert_eq!(new.label(), yaml.lines().nth(line));
                window.within(("new", line)).click(("line", line), cx);
                window.press(
                    if cfg!(target_os = "macos") {
                        "cmd-c"
                    } else {
                        "ctrl-c"
                    },
                    cx,
                );
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().unwrap(),
                    format!("{}\n", yaml.lines().nth(line).unwrap())
                );
                window.click("diff-unified", cx);
                window.render_frame(cx);
                window.click("diff-split", cx);
                window.render_frame(cx);
                window.render_frame(cx);
                assert!(window.within(("new", line)).find("source").visible());
            })
            .unwrap();
        }
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
