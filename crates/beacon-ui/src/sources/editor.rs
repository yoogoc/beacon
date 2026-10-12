use super::*;
use crate::{copyable_text::copyable_text, selection::checkbox, settings};
use beacon_kube::sources::{Kubeconfig, Preferences, Source, SourceKind};
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Paste,
}
struct Review {
    sources: Vec<Source>,
    catalog: Catalog,
    imported: Option<Arc<Kubeconfig>>,
    original: Preferences,
    inputs: Preferences,
}
pub(super) fn open(source: Option<Source>, rename: bool, window: &mut Window, cx: &mut App) {
    let editor = cx.new(|cx| SourceEditor::new(source, rename, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(if rename {
                "Rename source"
            } else {
                "Add kubeconfig source"
            })
            .width(px(600.))
            .overlay_closable(false)
            .child(editor.clone())
    });
}
struct SourceEditor {
    source: Option<Source>,
    rename: bool,
    kind: Kind,
    name: Entity<InputState>,
    path: Entity<InputState>,
    base: Entity<InputState>,
    patterns: Entity<InputState>,
    yaml: Entity<EditorState>,
    extra_files: Vec<PathBuf>,
    recursive: bool,
    review: Option<Review>,
    message: Option<String>,
    busy: bool,
    revision: u64,
    _operation: Option<Task<()>>,
    _inputs: Vec<Subscription>,
}
fn input(
    placeholder: &str,
    value: String,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder.to_owned())
            .default_value(value)
    })
}
fn new_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "source-{:x}-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
fn file_name(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
impl SourceEditor {
    fn new(
        source: Option<Source>,
        rename: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (kind, path, recursive, patterns) = match source.as_ref().map(|s| &s.kind) {
            Some(SourceKind::Directory {
                path,
                recursive,
                patterns,
            }) => (
                Kind::Directory,
                path.display().to_string(),
                *recursive,
                patterns.clone(),
            ),
            Some(SourceKind::File { path }) => (
                Kind::File,
                path.display().to_string(),
                false,
                sources::default_patterns(),
            ),
            Some(SourceKind::Imported) => (
                Kind::Paste,
                String::new(),
                false,
                sources::default_patterns(),
            ),
            None => (
                Kind::File,
                String::new(),
                false,
                sources::default_patterns(),
            ),
        };
        let name = input(
            "Source name",
            source.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
            window,
            cx,
        );
        let path = input("Choose a location or enter a path", path, window, cx);
        let base = input(
            "Optional base folder for relative file references",
            String::new(),
            window,
            cx,
        );
        let patterns = input("Filename patterns", patterns, window, cx);
        let yaml = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("yaml")
                .default_value("")
        });
        let mut inputs: Vec<_> = [&name, &path, &base, &patterns]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |view: &mut Self, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        view.revision += 1;
                        view.review = None;
                        view.message = None;
                        view.busy = false;
                        view._operation = None;
                        cx.notify();
                    }
                })
            })
            .collect();
        inputs.push(
            cx.subscribe(&yaml, |view: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view.revision += 1;
                    view.review = None;
                    view.message = None;
                    view.busy = false;
                    view._operation = None;
                    cx.notify();
                }
            }),
        );
        name.read(cx).focus_handle(cx).focus(window, cx);
        Self {
            source,
            rename,
            kind,
            name,
            path,
            base,
            patterns,
            yaml,
            extra_files: vec![],
            recursive,
            review: None,
            message: None,
            busy: false,
            revision: 0,
            _operation: None,
            _inputs: inputs,
        }
    }
    fn choose(&mut self, base: bool, window: &mut Window, cx: &mut Context<Self>) {
        let folder = base || self.kind == Kind::Directory;
        let multiple = !folder && self.source.is_none();
        let choosing = cx.prompt_for_paths(PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple,
            prompt: Some(
                if folder {
                    "Choose kubeconfig folder"
                } else {
                    "Choose kubeconfig files"
                }
                .into(),
            ),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = choosing.await
                && let Some(first) = paths.first()
            {
                let _ = cx.update(|window, cx| {
                    this.update(cx, |view, cx| {
                        let input = if base { &view.base } else { &view.path };
                        input.update(cx, |state, cx| {
                            state.set_value(first.display().to_string(), window, cx)
                        });
                        if !base {
                            view.extra_files = paths.iter().skip(1).cloned().collect();
                            if view.name.read(cx).value().trim().is_empty() {
                                view.name.update(cx, |state, cx| {
                                    state.set_value(file_name(first), window, cx)
                                });
                            }
                        }
                        cx.notify();
                    })
                });
            }
        })
        .detach();
    }
    fn drafts(&self, cx: &App) -> Result<Vec<Source>, String> {
        let name = self.name.read(cx).value().trim().to_string();
        let path = PathBuf::from(self.path.read(cx).value().trim());
        if self.kind != Kind::Paste && path.as_os_str().is_empty() {
            return Err("Choose a location first.".into());
        }
        if name.is_empty() {
            return Err("Give this source a name.".into());
        }
        let id = self
            .source
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or_else(new_id);
        let kind = match self.kind {
            Kind::File => SourceKind::File { path },
            Kind::Directory => SourceKind::Directory {
                path,
                recursive: self.recursive,
                patterns: self.patterns.read(cx).value().to_string(),
            },
            Kind::Paste => SourceKind::Imported,
        };
        let mut sources = vec![Source {
            id,
            name,
            kind,
            enabled: true,
            excluded: self
                .source
                .as_ref()
                .map(|s| s.excluded.clone())
                .unwrap_or_default(),
        }];
        for path in &self.extra_files {
            sources.push(Source {
                id: new_id(),
                name: file_name(path),
                kind: SourceKind::File { path: path.clone() },
                enabled: true,
                excluded: BTreeSet::new(),
            });
        }
        Preferences {
            system_enabled: false,
            sources: sources.clone(),
        }
        .validate()?;
        Ok(sources)
    }
    fn preview(&mut self, cx: &mut Context<Self>) {
        let drafts = match self.drafts(cx) {
            Ok(drafts) => drafts,
            Err(error) => {
                self.message = Some(error);
                cx.notify();
                return;
            }
        };
        let original = settings::store(cx).read(cx).preferences.kubeconfigs.clone();
        let mut inputs = original.clone();
        if let Some(source) = &self.source {
            inputs.sources.retain(|s| s.id != source.id);
        }
        inputs.sources.extend(drafts.clone());
        let directory = settings::store(cx).read(cx).directory.clone();
        let system = store(cx).read(cx).system.clone();
        let text = (self.kind == Kind::Paste).then(|| self.yaml.read(cx).value().to_string());
        let base = self.base.read(cx).value().trim().to_string();
        self.revision += 1;
        let revision = self.revision;
        self.busy = true;
        self.message = None;
        let loading = cx.background_executor().spawn(async move {
            let (catalog, imported) = if let Some(text) = text {
                let config = Arc::new(sources::pasted(
                    &text,
                    (!base.is_empty()).then(|| Path::new(&base)),
                )?);
                let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
                // Stage only in an owner-only temporary directory. The saved
                // configuration is written after the review is confirmed.
                sources::save_import(temporary.path(), &drafts[0].id, &config)?;
                let mut staged = inputs.clone();
                let path = sources::imported_path(temporary.path(), &drafts[0].id);
                staged
                    .sources
                    .iter_mut()
                    .find(|s| s.id == drafts[0].id)
                    .unwrap()
                    .kind = SourceKind::File { path };
                let mut catalog = sources::load(&staged, &directory, &system);
                if let Some(status) = catalog.statuses.iter_mut().find(|s| s.id == drafts[0].id) {
                    status.location = sources::imported_path(&directory, &drafts[0].id)
                        .display()
                        .to_string();
                }
                (catalog, Some(config))
            } else {
                (sources::load(&inputs, &directory, &system), None)
            };
            Ok::<_, String>(Review {
                sources: drafts,
                catalog,
                imported,
                original,
                inputs,
            })
        });
        self._operation = Some(cx.spawn(async move |this, cx| {
            let result = loading.await;
            let _ = this.update(cx, |view, cx| {
                if view.revision != revision {
                    return;
                }
                view.busy = false;
                match result {
                    Ok(review) => view.review = Some(review),
                    Err(error) => view.message = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename {
            let mut preferences = settings::store(cx).read(cx).preferences.clone();
            let Some(source) = self.source.as_ref().and_then(|s| {
                preferences
                    .kubeconfigs
                    .sources
                    .iter_mut()
                    .find(|current| current.id == s.id)
            }) else {
                self.message = Some("This source no longer exists.".into());
                cx.notify();
                return;
            };
            source.name = self.name.read(cx).value().trim().to_string();
            match settings::save(preferences, window, cx) {
                Ok(()) => window.close_dialog(cx),
                Err(error) => self.message = Some(error),
            }
            cx.notify();
            return;
        }
        let Some(review) = self.review.take() else {
            return;
        };
        if !review.sources.iter().any(|s| {
            review
                .catalog
                .statuses
                .iter()
                .find(|status| status.id == s.id)
                .is_some_and(|s| s.contexts.iter().any(|c| c.included))
        }) && self.source.is_none()
        {
            self.review = Some(review);
            self.message = Some("Select at least one valid context.".into());
            cx.notify();
            return;
        }
        let directory = settings::store(cx).read(cx).directory.clone();
        let system = store(cx).read(cx).system.clone();
        let replacing = self.source.clone();
        self.busy = true;
        self.message = None;
        let saving = cx.background_executor().spawn(async move {
            if review.imported.is_none() {
                let current = sources::load(&review.inputs, &directory, &system);
                if current.input_fingerprint != review.catalog.input_fingerprint {
                    return Err("The source files changed during review. Preview contexts again before saving.".to_string());
                }
            }
            let mut drafts = review.sources;
            if replacing.is_none() {
                drafts.retain(|draft| review.catalog.statuses.iter().find(|s| s.id == draft.id).is_some_and(|s| s.contexts.iter().any(|c| c.included)));
            }
            for draft in &mut drafts {
                if let Some(previous) = &replacing { draft.enabled = previous.enabled; }
            }
            let import = review.imported.as_ref()
                .map(|config| sources::stage_import(&directory, &drafts[0].id, config)).transpose()?;
            Ok((drafts, review.original, import, replacing))
        });
        self._operation = Some(cx.spawn_in(window, async move |this, cx| {
            let result = saving.await;
            let _ = cx.update(|window, cx| {
                this.update(cx, |view, cx| {
                    view.busy = false;
                    let result = result.and_then(|(drafts, original, mut import, replacing)| {
                        let mut preferences = settings::store(cx).read(cx).preferences.clone();
                        if preferences.kubeconfigs != original {
                            return Err(
                                "Sources changed in another window. Preview again before saving."
                                    .into(),
                            );
                        }
                        if let Some(import) = &mut import {
                            import.install()?;
                        }
                        if let Some(previous) = &replacing {
                            preferences
                                .kubeconfigs
                                .sources
                                .retain(|s| s.id != previous.id);
                        }
                        preferences.kubeconfigs.sources.extend(drafts);
                        match settings::save(preferences, window, cx) {
                            Ok(()) => {
                                window.close_dialog(cx);
                                Ok(())
                            }
                            Err(error) => {
                                let rollback =
                                    import.as_mut().and_then(|import| import.rollback().err());
                                Err(rollback.map_or(error.clone(), |rollback| {
                                    format!("{error}\n{rollback}")
                                }))
                            }
                        }
                    });
                    view.message = result.err();
                    cx.notify();
                })
            });
        }));
        cx.notify();
    }
    fn field(label: &str, input: &Entity<InputState>) -> AnyElement {
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(label.to_owned()),
            )
            .child(Input::new(input).small())
            .into_any_element()
    }
    fn form(&self, cx: &mut Context<Self>) -> AnyElement {
        let name = Self::field("Source name", &self.name);
        if self.rename {
            return name;
        }
        v_flex().gap_4()
            .child(h_flex().gap_2().children([(Kind::File,"File",IconName::FileKey),(Kind::Directory,"Folder",IconName::FolderOpen),(Kind::Paste,"Paste YAML",IconName::ClipboardPaste)].into_iter().map(|(kind,label,icon)| {
                Button::new(SharedString::from(format!("source-type-{label}"))).small().outline().icon(icon).label(label).selected(self.kind == kind).disabled(self.busy || self.source.is_some() && self.kind != kind)
                    .on_click(cx.listener(move |view, _, _, cx| { view.kind = kind; view.extra_files.clear(); view.review = None; view.message = None; cx.notify(); }))
            })))
            .child(name)
            .when(self.kind != Kind::Paste, |v| v.child(v_flex().gap_2()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(if self.kind == Kind::Directory { "Folder" } else { "Kubeconfig file" }))
                .child(h_flex().gap_2().items_center().child(div().flex_1().min_w_0().child(Input::new(&self.path).small()))
                    .child(Button::new("browse-kubeconfig").small().outline().label("Browse…").on_click(cx.listener(|view, _, window, cx| view.choose(false, window, cx)))))
                .children(self.extra_files.iter().enumerate().map(|(index, path)| h_flex().gap_2().items_center()
                    .child(div().flex_1().min_w_0().text_xs().child(copyable_text(("extra-source-path",index), path.display().to_string())))
                    .child(Button::new(("remove-source-file",index)).xsmall().ghost().icon(IconName::X).tooltip("Remove this file from the selection").on_click(cx.listener(move |view, _, _, cx| { view.extra_files.remove(index); cx.notify(); })))))))
            .when(self.kind == Kind::Directory, |v| v.child(Self::field("File names", &self.patterns))
                .child(checkbox("recursive-kubeconfig-scan").label("Include subfolders").checked(self.recursive).on_click(cx.listener(|view, value, _, cx| { view.recursive = *value; cx.notify(); }))))
            .when(self.kind == Kind::Paste, |v| v.child(v_flex().gap_2().child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Kubeconfig YAML"))
                .child(div().h(px(190.)).border_1().border_color(cx.theme().border).rounded(cx.theme().radius).child(Editor::new(&self.yaml).h_full())))
                .child(Self::field("Base folder (only for relative file references)", &self.base))
                .child(Button::new("browse-kubeconfig-base").small().ghost().icon(IconName::FolderOpen).label("Choose base folder…").on_click(cx.listener(|view, _, window, cx| view.choose(true, window, cx)))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(match self.kind { Kind::File => "Choose one or more files. Each file becomes a separate source and the originals stay in place.", Kind::Directory => "Only matching kubeconfig files are loaded. Invalid files do not block the rest of the folder.", Kind::Paste => "Beacon keeps an owner-only copy locally. Credentials and exec helpers are not used during this preview." }))
            .into_any_element()
    }
    fn review_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let review = self.review.as_ref().unwrap();
        v_flex().gap_4().children(review.sources.iter().map(|source| {
            let status = review.catalog.statuses.iter().find(|s| s.id == source.id).unwrap();
            let source_id = source.id.clone();
            v_flex().gap_2()
                .child(div().font_weight(FontWeight::MEDIUM).child(source.name.clone()))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(copyable_text(SharedString::from(format!("review-source-path-{}", source.id)), status.location.clone())))
                .children(status.issues.iter().enumerate().map(|(index, issue)| div().text_sm().text_color(cx.theme().warning).child(copyable_text((SharedString::from(format!("review-source-issue-{}", source.id)), index), format!("{}: {}", issue.file.display(), issue.message)))))
                .children(status.contexts.iter().map(|context| {
                    let id = source_id.clone(); let key = context.key.clone();
                    v_flex().gap_1().py_2().border_b_1().border_color(cx.theme().border)
                        .child(checkbox(SharedString::from(format!("include-context-{}-{}", id, key))).label(context.entry.id.context_name().to_owned()).checked(context.included)
                            .on_click(cx.listener(move |view, value, _, cx| {
                                if let Some(review) = &mut view.review {
                                    if let Some(source) = review.sources.iter_mut().find(|s| s.id == id) { if *value { source.excluded.remove(&key); } else { source.excluded.insert(key.clone()); } }
                                    if let Some(status) = review.catalog.statuses.iter_mut().find(|s| s.id == id) && let Some(context) = status.contexts.iter_mut().find(|c| c.key == key) { context.included = *value; }
                                }
                                cx.notify();
                            })))
                        .child(div().pl_6().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} · {} · {}", context.entry.initial_namespace(), context.authentication, context.file.file_name().unwrap_or_default().to_string_lossy())))
                }))
        })).child(div().text_xs().text_color(cx.theme().muted_foreground).child("Contexts with the same name stay separate. Adding a source does not connect a cluster; exec credential helpers run only when you connect."))
            .into_any_element()
    }
}
impl Render for SourceEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reviewed = self.review.is_some();
        let count = self
            .review
            .as_ref()
            .map(|r| {
                r.catalog
                    .statuses
                    .iter()
                    .filter(|s| r.sources.iter().any(|source| source.id == s.id))
                    .flat_map(|s| &s.contexts)
                    .filter(|c| c.included)
                    .count()
            })
            .unwrap_or_default();
        v_flex()
            .id("kubeconfig-source-editor")
            .test_support()
            .gap_4()
            .w_full()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.rename {
                        "Changes apply to the source label in all windows."
                    } else if reviewed {
                        "2 of 2 · Choose which contexts appear in Beacon"
                    } else {
                        "1 of 2 · Choose a source"
                    }),
            )
            .child(if reviewed {
                self.review_form(cx)
            } else {
                self.form(cx)
            })
            .when_some(self.message.clone(), |v, message| {
                v.child(
                    div()
                        .id("source-editor-error")
                        .test_support()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(copyable_text("source-editor-error", message)),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .items_center()
                    .child(
                        Button::new("cancel-source-editor")
                            .small()
                            .outline()
                            .label(if reviewed { "Back" } else { "Cancel" })
                            .disabled(self.busy)
                            .on_click(cx.listener(|view, _, window, cx| {
                                if view.review.is_some() {
                                    view.review = None;
                                    view.message = None;
                                    cx.notify();
                                } else {
                                    window.close_dialog(cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("confirm-source-editor")
                            .small()
                            .primary()
                            .label(if self.busy {
                                "Working…"
                            } else if self.rename {
                                "Save name"
                            } else if reviewed {
                                if self.source.is_some() {
                                    "Save source"
                                } else {
                                    "Add source"
                                }
                            } else {
                                "Preview contexts"
                            })
                            .disabled(self.busy || reviewed && count == 0 && self.source.is_none())
                            .on_click(cx.listener(move |view, _, window, cx| {
                                if reviewed || view.rename {
                                    view.commit(window, cx);
                                } else {
                                    view.preview(cx);
                                }
                            })),
                    ),
            )
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    fn yaml() -> &'static str {
        "apiVersion: v1\nkind: Config\nclusters:\n- name: demo\n  cluster:\n    server: https://mock.example.com\ncontexts:\n- name: dev\n  context:\n    cluster: demo\n- name: staging\n  context:\n    cluster: demo\n"
    }
    fn editor(cx: &mut TestAppContext, path: &Path) -> (AnyWindowHandle, Entity<SourceEditor>) {
        let window = support::window(cx);
        let editor = cx
            .update_window(window, |_, window, cx| {
                let editor = cx.new(|cx| SourceEditor::new(None, false, window, cx));
                editor.update(cx, |view, cx| {
                    view.name
                        .update(cx, |state, cx| state.set_value("Team", window, cx));
                    view.path.update(cx, |state, cx| {
                        state.set_value(path.display().to_string(), window, cx)
                    });
                });
                let content = editor.clone();
                window.open_dialog(cx, move |dialog, _, _| {
                    dialog.title("Add source").child(content.clone())
                });
                editor
            })
            .unwrap();
        (window, editor)
    }
    #[::core::prelude::v1::test]
    fn import_requires_review_preserves_original_and_never_connects() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let file = directory.path().join("team.yaml");
        std::fs::write(&file, yaml()).unwrap();
        let (window, editor) = editor(cx, &file);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("confirm-source-editor", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            editor.read_with(cx, |editor, _| editor.review.is_some())
        });
        assert!(cx.read(|cx| {
            settings::store(cx)
                .read(cx)
                .preferences
                .kubeconfigs
                .sources
                .is_empty()
        }));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let review = editor.read(cx).review.as_ref().unwrap();
            let source = &review.sources[0];
            let key = &review
                .catalog
                .statuses
                .iter()
                .find(|s| s.id == source.id)
                .unwrap()
                .contexts[0]
                .key;
            window.click(
                SharedString::from(format!("include-context-{}-{}", source.id, key)),
                cx,
            );
            window.render_frame(cx);
            window.click("confirm-source-editor", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            cx.read(|cx| {
                settings::store(cx)
                    .read(cx)
                    .preferences
                    .kubeconfigs
                    .sources
                    .len()
                    == 1
            })
        });
        support::settle(cx, |cx| {
            cx.read(|cx| store(cx).read(cx).catalog.targets.len() == 1)
        });
        assert_eq!(std::fs::read_to_string(&file).unwrap(), yaml());
        assert!(cx.read(|cx| {
            cx.global::<crate::connections::SharedConnections>()
                .0
                .read(cx)
                .sessions
                .is_empty()
        }));
        assert_eq!(
            cx.read(|cx| store(cx).read(cx).catalog.contexts.entries()[0]
                .id
                .context_name()
                .to_string()),
            "staging"
        );
        cx.update_window(window, |_, window, cx| {
            assert!(!window.has_active_dialog(cx))
        })
        .unwrap();
    }
    #[::core::prelude::v1::test]
    fn source_changes_between_preview_and_confirm_require_another_review() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let file = directory.path().join("team.yaml");
        std::fs::write(&file, yaml()).unwrap();
        let (window, editor) = editor(cx, &file);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("confirm-source-editor", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            editor.read_with(cx, |editor, _| editor.review.is_some())
        });
        std::fs::write(
            &file,
            yaml().replace("mock.example.com", "changed.example.com"),
        )
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("confirm-source-editor", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            editor.read_with(cx, |editor, _| {
                editor
                    .message
                    .as_deref()
                    .is_some_and(|m| m.contains("changed during review"))
            })
        });
        assert!(cx.read(|cx| {
            settings::store(cx)
                .read(cx)
                .preferences
                .kubeconfigs
                .sources
                .is_empty()
        }));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("source-editor-error").visible());
        })
        .unwrap();
    }
    #[::core::prelude::v1::test]
    fn pasted_credentials_are_stored_only_after_confirmation() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let (window, editor) = editor(cx, Path::new("unused"));
        let text = yaml().to_string()
            + "users:\n- name: demo\n  user:\n    token: preview-only-credential\n";
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |view, cx| {
                view.kind = Kind::Paste;
                view.yaml
                    .update(cx, |state, cx| state.set_value(text.clone(), window, cx));
            });
            window.render_frame(cx);
            window.click("confirm-source-editor", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            editor.read_with(cx, |editor, _| editor.review.is_some())
        });
        assert!(!directory.path().join("kubeconfigs").exists());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("confirm-source-editor", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            cx.read(|cx| {
                settings::store(cx)
                    .read(cx)
                    .preferences
                    .kubeconfigs
                    .sources
                    .len()
                    == 1
            })
        });
        let source =
            cx.read(|cx| settings::store(cx).read(cx).preferences.kubeconfigs.sources[0].clone());
        assert!(
            std::fs::read_to_string(sources::imported_path(directory.path(), &source.id))
                .unwrap()
                .contains("preview-only-credential")
        );
        assert!(
            !std::fs::read_to_string(directory.path().join("settings.json"))
                .unwrap()
                .contains("preview-only-credential")
        );
        assert!(cx.read(|cx| {
            cx.global::<crate::connections::SharedConnections>()
                .0
                .read(cx)
                .sessions
                .is_empty()
        }));
    }
}
