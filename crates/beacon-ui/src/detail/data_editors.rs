use super::*;

pub(super) struct HeightDrag {
    pub id: String,
    pub height: Pixels,
    pub y: Pixels,
}

pub(super) fn id(entry: &DataEntry) -> String {
    format!("{}:{}", entry.field.name(), entry.key)
}

impl DetailView {
    pub(super) fn prepare_data_syntax(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind.resource.kind == "Secret" && !self.revealed {
            return;
        }
        let ids = match &self.data {
            Data::Ready(keys) => keys
                .iter()
                .filter(|key| key.editor.is_some())
                .map(|key| id(&key.entry))
                .collect::<Vec<_>>(),
            Data::Unopened => vec![],
        };
        for id in ids {
            self.schedule_data_syntax(id, window, cx);
        }
    }

    pub(super) fn schedule_data_syntax(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.kind.resource.kind == "Secret" && !self.revealed {
            return;
        }
        let Data::Ready(keys) = &mut self.data else {
            return;
        };
        let Some(key) = keys.iter_mut().find(|key| self::id(&key.entry) == id) else {
            return;
        };
        key._detect = Some(cx.spawn_in(window, async move |view, cx| {
            // Debounce typing and perform content parsing off the foreground thread.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            let Some((name, text, editor)) = view
                .update(cx, |view, cx| {
                    let Data::Ready(keys) = &view.data else {
                        return None;
                    };
                    let key = keys.iter().find(|key| self::id(&key.entry) == id)?;
                    let editor = key.editor.clone()?;
                    Some((
                        key.entry.key.clone(),
                        editor.read(cx).value().to_string(),
                        editor,
                    ))
                })
                .ok()
                .flatten()
            else {
                return;
            };
            let sample = text.clone();
            let analysis = cx
                .background_executor()
                .spawn(async move { crate::data_syntax::analyze(&name, &sample) })
                .await;
            let _ = view.update(cx, |view, cx| {
                if view.kind.resource.kind == "Secret" && !view.revealed {
                    return;
                }
                let Data::Ready(keys) = &mut view.data else {
                    return;
                };
                let Some(key) = keys.iter_mut().find(|key| self::id(&key.entry) == id) else {
                    return;
                };
                if key.editor.as_ref() != Some(&editor) || editor.read(cx).value().as_ref() != text
                {
                    return;
                }
                if editor.read(cx).language_name().as_ref() != analysis.format.language() {
                    editor.update(cx, |state, cx| {
                        state.set_highlighter(analysis.format.language(), cx)
                    });
                }
                if let Some(decorations) = &key.decorations {
                    decorations.set(
                        crate::data_syntax::decorations(&analysis.tokens, cx.theme()),
                        cx,
                    );
                }
                key.syntax = analysis;
                cx.notify();
            });
        }));
    }

    pub(super) fn refresh_data_colors(&self, cx: &mut Context<Self>) {
        if let Data::Ready(keys) = &self.data {
            for key in keys {
                if let Some(decorations) = &key.decorations {
                    decorations.set(
                        crate::data_syntax::decorations(&key.syntax.tokens, cx.theme()),
                        cx,
                    );
                }
            }
        }
    }

    pub(super) fn data_resize_listener(&self, cx: &Context<Self>) -> AnyElement {
        let view = cx.weak_entity();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let moving = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if !phase.capture() {
                        return;
                    }
                    let _ = moving.update(cx, |view, cx| {
                        let Some(drag) = &view.data_resize else {
                            return;
                        };
                        if event.pressed_button != Some(MouseButton::Left) {
                            view.data_resize = None;
                            cx.notify();
                            return;
                        }
                        let height =
                            (drag.height + event.position.y - drag.y).clamp(px(64.), px(1000.));
                        if view.data_heights.get(&drag.id) != Some(&height) {
                            view.data_heights.insert(drag.id.clone(), height);
                            cx.notify();
                        }
                    });
                });
                let view = view.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase.capture() && event.button == MouseButton::Left {
                        let _ = view.update(cx, |view, cx| {
                            if view.data_resize.take().is_some() {
                                cx.notify();
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::{data_syntax::Format, feature_test_support as support};
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    use std::time::Duration;

    fn key<'a>(view: &'a DetailView, name: &str) -> &'a DataKey {
        let Data::Ready(keys) = &view.data else {
            panic!("Data not loaded")
        };
        keys.iter().find(|key| key.entry.key == name).unwrap()
    }

    #[track_caller]
    fn detect(cx: &mut TestAppContext, view: &Entity<DetailView>, name: &str, format: Format) {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(200));
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| {
                let key = key(view, name);
                key.syntax.format == format
                    && key
                        .editor
                        .as_ref()
                        .unwrap()
                        .read(cx)
                        .language_name()
                        .as_ref()
                        == format.language()
            })
        });
    }

    fn open(
        cx: &mut TestAppContext,
        session: Arc<ClusterSession>,
        kind: &str,
        object: Value,
    ) -> (AnyWindowHandle, Entity<DetailView>) {
        let plural = if kind == "Secret" {
            "secrets"
        } else {
            "configmaps"
        };
        let mut object: DynamicObject = serde_json::from_value(object).unwrap();
        object.types = None;
        let object = Arc::new(object);
        cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(1000.), px(1000.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| {
                        let mut view = DetailView::new(
                            session,
                            Arc::new(support::kind("", kind, plural)),
                            object,
                            None,
                            window,
                            cx,
                        );
                        view.select(DetailTab::Data, window, cx);
                        view
                    })
                },
            )
            .unwrap()
        })
    }

    fn fold_and_unfold(view: &Entity<DetailView>, name: &str, window: &mut Window, cx: &mut App) {
        let entry = key(view.read(cx), name);
        let scope = SharedString::from(format!("data-editor-{}", id(&entry.entry)));
        let editor = entry.editor.clone().unwrap();
        let original = editor.read(cx).value();
        editor.read(cx).focus_handle(cx).focus(window, cx);
        let start = if cfg!(target_os = "macos") {
            "cmd-up"
        } else {
            "ctrl-home"
        };
        window.press(start, cx);
        // Exercise the actual parser-provided gutter control, without supplying
        // artificial fold ranges or changing the buffer contents.
        window
            .within(scope.clone())
            .click(("fold-icon", 0usize), cx);
        window.press("down", cx);
        assert!(editor.read(cx).cursor_position().line > 1);
        assert_eq!(editor.read(cx).value(), original);
        assert!(!view.read(cx).has_pending_edits(cx));
        window.within(scope).click(("fold-icon", 0usize), cx);
        window.press(start, cx);
        window.press("down", cx);
        assert_eq!(editor.read(cx).cursor_position().line, 1);
        assert_eq!(editor.read(cx).value(), original);
    }

    #[::core::prelude::v1::test]
    fn configmap_content_highlights_and_resize_preserve_drafts_and_undo() {
        let cx = &mut support::context();
        let json = "{\n  \"enabled\": true,\n  \"name\": \"配置\"\n}\n";
        let ini = "# 说明\n[server]\nname=\"应用\"\nport=80\n";
        let object = json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"syntax", "namespace":"default", "uid":"syntax"}, "data":{"settings":json, "server.ini":ini}});
        let (_fixture, session) = support::fixture(cx, "data-syntax", vec![object.clone()]);
        let (window, view) = open(cx, session, "ConfigMap", object);
        detect(cx, &view, "settings", Format::Json);
        detect(cx, &view, "server.ini", Format::Ini);
        view.read_with(cx, |view, cx| {
            assert!(
                !key(view, "server.ini")
                    .decorations
                    .as_ref()
                    .unwrap()
                    .get_ranges(cx)
                    .is_empty()
            );
            assert!(!view.has_pending_edits(cx));
        });
        let (height, other_height) = view.read_with(cx, |view, _| {
            (
                view.data_heights["data:settings"],
                view.data_heights["data:server.ini"],
            )
        });
        cx.update_window(window, |_, window, cx| {
            fold_and_unfold(&view, "settings", window, cx);
            window.render_frame(cx);
            let before = window
                .find("data-editor-data:settings")
                .bounds()
                .size
                .height;
            let handle = window.find("resize-data-data:settings").bounds().center();
            window.drag(handle, handle + point(px(0.), px(120.)), cx);
            window.render_frame(cx);
            assert_eq!(
                window
                    .find("data-editor-data:settings")
                    .bounds()
                    .size
                    .height,
                before + px(120.)
            );
            assert_eq!(
                view.read(cx).data_heights["data:settings"],
                height + px(120.)
            );
            assert_eq!(view.read(cx).data_heights["data:server.ini"], other_height);
            assert!(view.read(cx).data_resize.is_none());
            let handle = window.find("resize-data-data:settings").bounds().center();
            window.drag(handle, handle - point(px(0.), px(1000.)), cx);
            assert_eq!(view.read(cx).data_heights["data:settings"], px(64.));
            view.update(cx, |view, cx| {
                view.select(DetailTab::Overview, window, cx);
                view.select(DetailTab::Data, window, cx);
            });
            assert_eq!(view.read(cx).data_heights["data:settings"], px(64.));
            let editor = key(view.read(cx), "settings").editor.clone().unwrap();
            assert_eq!(editor.read(cx).value().as_ref(), json);
            editor.read(cx).focus_handle(cx).focus(window, cx);
            window.press(
                if cfg!(target_os = "macos") {
                    "cmd-a"
                } else {
                    "ctrl-a"
                },
                cx,
            );
            cx.write_to_clipboard(ClipboardItem::new_string(
                "enabled: false\nname: 配置\n".into(),
            ));
            window.press(
                if cfg!(target_os = "macos") {
                    "cmd-v"
                } else {
                    "ctrl-v"
                },
                cx,
            );
            assert_eq!(
                editor.read(cx).value().as_ref(),
                "enabled: false\nname: 配置\n"
            );
        })
        .unwrap();
        detect(cx, &view, "settings", Format::Yaml);
        view.read_with(cx, |view, cx| {
            assert_eq!(
                key(view, "settings")
                    .editor
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_ref(),
                "enabled: false\nname: 配置\n"
            );
            assert!(view.has_pending_edits(cx));
        });
        cx.update_window(window, |_, window, cx| {
            window.press(
                if cfg!(target_os = "macos") {
                    "cmd-z"
                } else {
                    "ctrl-z"
                },
                cx,
            )
        })
        .unwrap();
        view.read_with(cx, |view, cx| {
            assert_eq!(
                key(view, "settings")
                    .editor
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_ref(),
                json
            );
        });
        detect(cx, &view, "settings", Format::Json);
        view.read_with(cx, |view, cx| {
            assert_eq!(
                key(view, "settings")
                    .editor
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_ref(),
                json
            );
            assert!(!view.has_pending_edits(cx));
        });
    }

    #[::core::prelude::v1::test]
    fn secret_highlights_decoded_text_only_after_reveal_and_preserves_binary_values() {
        let cx = &mut support::context();
        let text = "{\n  \"auth\": \"配置\",\n  \"enabled\": true\n}\n";
        let encoded = data::encode(beacon_kube::DataField::Data, true, text);
        let object = json!({"apiVersion":"v1", "kind":"Secret", "metadata":{"name":"syntax", "namespace":"default", "uid":"syntax"}, "type":"Opaque", "data":{"settings":encoded, "binary":"/w=="}});
        let (fixture, session) = support::fixture(cx, "secret-syntax", vec![object.clone()]);
        let (window, view) = open(cx, session, "Secret", object);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("data-editor-data:settings").is_none());
            assert!(window.try_find("resize-data-data:settings").is_none());
            assert!(key(view.read(cx), "binary").editor.is_none());
            assert!(key(view.read(cx), "settings")._detect.is_none());
            window.click("reveal", cx);
        })
        .unwrap();
        detect(cx, &view, "settings", Format::Json);
        cx.update_window(window, |_, window, cx| {
            fold_and_unfold(&view, "settings", window, cx);
            window.render_frame(cx);
            let height = window
                .find("data-editor-data:settings")
                .bounds()
                .size
                .height;
            let handle = window.find("resize-data-data:settings").bounds().center();
            window.drag(handle, handle + point(px(0.), px(100.)), cx);
            window.render_frame(cx);
            assert_eq!(
                window
                    .find("data-editor-data:settings")
                    .bounds()
                    .size
                    .height,
                height + px(100.)
            );
            assert_eq!(
                key(view.read(cx), "settings")
                    .editor
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_ref(),
                text
            );
            assert!(!view.read(cx).has_pending_edits(cx));
            window.click("reveal", cx);
            window.render_frame(cx);
            assert!(window.try_find("data-editor-data:settings").is_none());
            assert!(window.try_find("resize-data-data:binary").is_none());
        })
        .unwrap();
        assert_eq!(
            fixture.object("Secret", "syntax")["data"]["settings"],
            encoded
        );
        assert_eq!(fixture.object("Secret", "syntax")["data"]["binary"], "/w==");
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("PATCH"))
        );
    }
}
