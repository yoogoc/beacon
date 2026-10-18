use super::*;
use gpui_kit::component::input::InputState;
use std::{cell::Cell, collections::VecDeque, rc::Rc};

/// Rows and drafts stay lightweight. Only visible rows are rendered, and only
/// opened values receive editors. Keep a small clean-editor cache for undo and
/// cursor positions; modified editors are never evicted.
pub(super) struct Browser {
    pub selected: Option<usize>,
    selected_key: Option<String>,
    pub visible: Arc<Vec<usize>>,
    pub modified: BTreeSet<usize>,
    pub deleted: BTreeSet<usize>,
    pub search: Option<Entity<InputState>>,
    pub scroll: UniformListScrollHandle,
    pub width: Rc<Cell<Pixels>>,
    pub wrap: bool,
    pub details: bool,
    pub recent: VecDeque<usize>,
    _search: Option<Subscription>,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            selected: None,
            selected_key: None,
            visible: Arc::new(vec![]),
            modified: BTreeSet::new(),
            deleted: BTreeSet::new(),
            search: None,
            scroll: UniformListScrollHandle::default(),
            width: Rc::new(Cell::new(px(440.))),
            wrap: false,
            details: true,
            recent: VecDeque::new(),
            _search: None,
        }
    }
}

pub(super) struct HeightDrag {
    pub id: String,
    pub height: Pixels,
    pub y: Pixels,
}

pub(super) fn id(entry: &DataEntry) -> String {
    format!("{}:{}", entry.field.name(), entry.key)
}

impl DetailView {
    pub(super) fn load_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.data_browser.selected_key.take();
        // The Data tab uses the store object without a network request. Pin the
        // baseline until save succeeds, including when watches refresh overview.
        self.data_baseline = serde_json::to_value(&*self.object).ok().map(|mut object| {
            object["apiVersion"] = Value::String(self.kind.resource.api_version.clone());
            object["kind"] = Value::String(self.kind.resource.kind.clone());
            object
        });
        let mut keys: Vec<_> = data::read(&self.object, self.kind.resource.kind == "Secret")
            .into_iter()
            .map(|entry| DataKey {
                entry,
                editor: None,
                syntax: crate::data_syntax::Analysis {
                    format: crate::data_syntax::Format::Plain,
                    tokens: vec![],
                },
                decorations: None,
                inspection: None,
                inspection_value: None,
                _change: None,
                _detect: None,
            })
            .collect();
        if crate::service_account_secret::supported(
            &self.kind.resource.group,
            &self.kind.resource.kind,
            &self.object,
        ) {
            keys.sort_by_key(|key| {
                crate::service_account_secret::Key::of(&key.entry.key).map_or(3, |key| key.order())
            });
        }
        self.data_browser.selected = selected
            .and_then(|id| keys.iter().position(|key| self::id(&key.entry) == id))
            .or_else(|| keys.iter().position(|key| key.entry.text.is_some()))
            .or_else(|| (!keys.is_empty()).then_some(0));
        self.data_browser.modified.clear();
        self.data_browser.deleted.clear();
        self.data_browser.recent.clear();
        self.data_resize = None;
        self.data = Data::Ready(keys);
        if self.data_browser.search.is_none() {
            let search =
                cx.new(|cx| crate::i18n::input("Search keys…", window, cx).clean_on_escape());
            self.data_browser._search = Some(cx.subscribe_in(
                &search,
                window,
                |view, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        view.filter_data(window, cx);
                    }
                },
            ));
            self.data_browser.search = Some(search);
        }
        if self._data_theme.is_none() {
            self._data_theme = Some(cx.observe_global::<gpui_kit::component::Theme>(|view, cx| {
                view.refresh_data_colors(cx)
            }));
        }
        self.filter_data(window, cx);
    }

    fn filter_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_data_changes(cx);
        let Data::Ready(keys) = &self.data else {
            return;
        };
        let query = self.data_browser.search.as_ref().unwrap().read(cx).value();
        let query = query.trim().to_lowercase();
        self.data_browser.visible = Arc::new(
            keys.iter()
                .enumerate()
                .filter(|(_, key)| {
                    query.is_empty() || key.entry.key.to_lowercase().contains(&query)
                })
                .map(|(index, _)| index)
                .collect(),
        );
        let index = self
            .data_browser
            .selected
            .filter(|index| self.data_browser.visible.contains(index))
            .or_else(|| self.data_browser.visible.first().copied());
        self.data_browser.selected = index;
        self.data_browser.selected_key = index.map(|index| id(&keys[index].entry));
        self.open_selected_data(window, cx);
        if let Some(row) =
            index.and_then(|index| self.data_browser.visible.iter().position(|i| *i == index))
        {
            self.data_browser
                .scroll
                .scroll_to_item(row, ScrollStrategy::Top);
        }
        cx.notify();
    }

    pub(super) fn select_data_key(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_data_changes(cx);
        let Data::Ready(keys) = &self.data else {
            return;
        };
        if index >= keys.len() {
            return;
        }
        TextSelection::end(window, cx);
        self.data_resize = None;
        self.data_browser.selected = Some(index);
        self.data_browser.selected_key = Some(id(&keys[index].entry));
        self.open_selected_data(window, cx);
        cx.notify();
    }

    pub(super) fn sync_data_changes(&mut self, cx: &App) {
        let Data::Ready(keys) = &self.data else {
            return;
        };
        for &index in &self.data_browser.recent {
            let key = &keys[index];
            if key.editor.as_ref().is_some_and(|editor| {
                Some(editor.read(cx).value().as_ref()) != key.entry.text.as_deref()
            }) {
                self.data_browser.modified.insert(index);
            } else {
                self.data_browser.modified.remove(&index);
            }
        }
    }

    pub(super) fn open_selected_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind.resource.kind == "Secret" && !self.revealed {
            return;
        }
        let Some(index) = self.data_browser.selected else {
            return;
        };
        let inspect = crate::service_account_secret::supported(
            &self.kind.resource.group,
            &self.kind.resource.kind,
            &self.object,
        );
        if self.data_browser.deleted.contains(&index) {
            return;
        }
        let Data::Ready(keys) = &mut self.data else {
            return;
        };
        let key = &mut keys[index];
        let Some(text) = key.entry.text.as_ref() else {
            return;
        };
        let fresh = key.editor.is_none();
        let wrap = self.data_browser.wrap;
        if fresh {
            self.data_heights.entry(id(&key.entry)).or_insert_with(|| {
                px((text.lines().count().clamp(10, 20) as f32
                    * f32::from(cx.theme().mono_font_size)
                    * 1.5
                    + 24.)
                    .clamp(220., 440.))
            });
            let editor = cx.new(|cx| {
                let mut state = EditorState::new(window, cx)
                    .language("text")
                    .soft_wrap(wrap)
                    .auto_close(false)
                    .smart_indent(false)
                    .folding(true);
                state.set_value(text.clone(), window, cx);
                state
            });
            key.decorations = Some(editor.update(cx, |state, cx| {
                state.create_decorations_collection(vec![], cx)
            }));
            key._change = Some(cx.subscribe_in(
                &editor,
                window,
                move |view, editor, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        let Data::Ready(keys) = &mut view.data else {
                            return;
                        };
                        let Some(key) = keys.get_mut(index) else {
                            return;
                        };
                        if key.editor.as_ref() != Some(editor) {
                            return;
                        }
                        key.inspection = None;
                        key.inspection_value = None;
                        if Some(editor.read(cx).value().as_ref()) == key.entry.text.as_deref() {
                            view.data_browser.modified.remove(&index);
                        } else {
                            view.data_browser.modified.insert(index);
                        }
                        view.schedule_data_syntax(index, window, cx);
                        cx.notify();
                    }
                },
            ));
            key.editor = Some(editor);
        }
        self.data_browser.recent.retain(|i| *i != index);
        self.data_browser.recent.push_front(index);
        let mut clean = 0;
        self.data_browser.recent.retain(|i| {
            if self.data_browser.modified.contains(i) {
                return true;
            }
            clean += 1;
            if clean <= 8 {
                return true;
            }
            let key = &mut keys[*i];
            key._change = None;
            key._detect = None;
            key.decorations = None;
            key.editor = None;
            key.syntax.tokens = vec![];
            key.inspection = None;
            key.inspection_value = None;
            false
        });
        if fresh
            || keys[index]._detect.is_none()
            || (inspect
                && crate::service_account_secret::Key::of(&keys[index].entry.key).is_some()
                && keys[index].inspection.is_none())
        {
            self.schedule_data_syntax(index, window, cx);
        }
    }

    pub(super) fn toggle_data_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.data_browser.wrap = !self.data_browser.wrap;
        if let Data::Ready(keys) = &self.data {
            for &index in &self.data_browser.recent {
                if let Some(editor) = &keys[index].editor {
                    editor.update(cx, |state, cx| {
                        state.set_soft_wrap(self.data_browser.wrap, window, cx)
                    });
                }
            }
        }
        cx.notify();
    }

    pub(super) fn cover_data(&mut self) {
        self.data_resize = None;
        if let Data::Ready(keys) = &mut self.data {
            for &index in &self.data_browser.recent {
                keys[index]._detect = None;
                keys[index].inspection = None;
                keys[index].inspection_value = None;
            }
        }
    }

    pub(super) fn delete_data_key(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy() {
            return;
        }
        self.sync_data_changes(cx);
        let Data::Ready(keys) = &mut self.data else {
            return;
        };
        let Some(key) = keys.get_mut(index) else {
            return;
        };
        if self.data_browser.deleted.remove(&index) {
            self.open_selected_data(window, cx);
        } else {
            if !crate::actions::may_apply(&self.kind, self.rules.as_deref())
                || (self.kind.resource.kind == "Secret" && !self.revealed)
            {
                return;
            }
            self.data_browser.deleted.insert(index);
            key._detect = None;
            // SSA omission does not delete a field owned by another manager.
            // Key removal must be an explicit, version-checked remove operation.
            if self.save_method != SaveMethod::Patch {
                self.save_method = SaveMethod::Patch;
                self.apply = Apply::Idle;
            }
        }
        self.data_resize = None;
        cx.notify();
    }

    pub(super) fn schedule_data_syntax(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.kind.resource.kind == "Secret" && !self.revealed {
            return;
        }
        let Data::Ready(keys) = &mut self.data else {
            return;
        };
        if self.data_browser.deleted.contains(&index) {
            return;
        }
        let Some(key) = keys.get_mut(index) else {
            return;
        };
        key._detect = Some(cx.spawn_in(window, async move |view, cx| {
            // Debounce typing and perform content parsing off the foreground thread.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            let Some((name, text, editor, service_account)) = view
                .update(cx, |view, cx| {
                    if view.kind.resource.kind == "Secret" && !view.revealed {
                        return None;
                    }
                    let Data::Ready(keys) = &view.data else {
                        return None;
                    };
                    let key = keys.get(index)?;
                    let editor = key.editor.clone()?;
                    Some((
                        key.entry.key.clone(),
                        editor.read(cx).value().to_string(),
                        editor,
                        crate::service_account_secret::supported(
                            &view.kind.resource.group,
                            &view.kind.resource.kind,
                            &view.object,
                        ),
                    ))
                })
                .ok()
                .flatten()
            else {
                return;
            };
            let sample = text.clone();
            let (analysis, inspection) = cx
                .background_executor()
                .spawn(async move {
                    let analysis = crate::data_syntax::analyze(&name, &sample);
                    let inspection = if service_account {
                        crate::service_account_secret::inspect(&name, &sample).map(Box::new)
                    } else {
                        None
                    };
                    (analysis, inspection)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                if view.kind.resource.kind == "Secret" && !view.revealed {
                    return;
                }
                let Data::Ready(keys) = &mut view.data else {
                    return;
                };
                if view.data_browser.deleted.contains(&index) {
                    return;
                }
                let Some(key) = keys.get_mut(index) else {
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
                key.inspection_value = inspection.as_ref().map(|_| SharedString::from(text));
                key.inspection = inspection;
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
mod integration_tests;
