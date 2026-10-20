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
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

mod file_form;

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
struct FileDraft {
    id: String,
    path: PathBuf,
    name: Entity<InputState>,
    _input: Subscription,
}
pub(super) fn open(source: Option<Source>, rename: bool, window: &mut Window, cx: &mut App) {
    let editor = cx.new(|cx| SourceEditor::new(source, rename, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(crate::i18n::text(if rename {
                "Rename source"
            } else {
                "Add kubeconfig source"
            }))
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
    files: Vec<FileDraft>,
    manual_path: bool,
    file_scroll: ScrollHandle,
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
    cx.new(|cx| crate::i18n::input(placeholder.to_owned(), window, cx).default_value(value))
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
        let initial_file = (kind == Kind::File && !path.is_empty()).then(|| PathBuf::from(&path));
        let name = input(
            "Source name",
            source.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
            window,
            cx,
        );
        let path = input(
            "Choose a location or enter a path",
            if kind == Kind::File {
                String::new()
            } else {
                path
            },
            window,
            cx,
        );
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
                        view.invalidate(cx);
                    }
                })
            })
            .collect();
        inputs.push(
            cx.subscribe(&yaml, |view: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view.invalidate(cx);
                }
            }),
        );
        inputs.push(cx.subscribe_in(
            &path,
            window,
            |view: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. })
                    && view.kind == Kind::File
                    && view.manual_path
                {
                    view.add_entered_path(window, cx);
                }
            },
        ));
        if rename || kind != Kind::File {
            name.read(cx).focus_handle(cx).focus(window, cx);
        }
        let mut this = Self {
            source,
            rename,
            kind,
            name,
            path,
            base,
            patterns,
            yaml,
            files: vec![],
            manual_path: false,
            file_scroll: ScrollHandle::new(),
            recursive,
            review: None,
            message: None,
            busy: false,
            revision: 0,
            _operation: None,
            _inputs: inputs,
        };
        if let Some(path) = initial_file {
            this.add_files(vec![path], window, cx);
        }
        this
    }
    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        self.review = None;
        self.message = None;
        self.busy = false;
        self._operation = None;
        cx.notify();
    }

    fn add_files(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let replacing_name = self.source.as_ref().map(|source| {
            self.files.first().map_or_else(
                || source.name.clone(),
                |file| file.name.read(cx).value().to_string(),
            )
        });
        if self.source.is_some() {
            self.files.clear();
        }
        for path in paths {
            if self.files.iter().any(|file| file.path == path) {
                continue;
            }
            let name = input(
                "Source name",
                replacing_name.clone().unwrap_or_else(|| file_name(&path)),
                window,
                cx,
            );
            let subscription = cx.subscribe(&name, |view: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view.invalidate(cx);
                }
            });
            self.files.push(FileDraft {
                id: new_id(),
                path,
                name,
                _input: subscription,
            });
            if self.source.is_some() {
                break;
            }
        }
        self.invalidate(cx);
    }

    fn add_entered_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.path.read(cx).value().trim().to_string();
        if value.is_empty() {
            return;
        }
        self.add_files(vec![PathBuf::from(value)], window, cx);
        self.path
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.manual_path = false;
    }
    fn choose(&mut self, base: bool, window: &mut Window, cx: &mut Context<Self>) {
        let folder = base || self.kind == Kind::Directory;
        let multiple = !folder && self.source.is_none();
        let choosing = cx.prompt_for_paths(PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple,
            prompt: Some(crate::i18n::text(if folder {
                "Choose kubeconfig folder"
            } else {
                "Choose kubeconfig files"
            })),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = choosing.await
                && let Some(first) = paths.first().cloned()
            {
                let _ = cx.update(|window, cx| {
                    this.update(cx, |view, cx| {
                        if !base && view.kind == Kind::File {
                            view.add_files(paths, window, cx);
                            return;
                        }
                        let input = if base { &view.base } else { &view.path };
                        input.update(cx, |state, cx| {
                            state.set_value(first.display().to_string(), window, cx)
                        });
                        if !base && view.name.read(cx).value().trim().is_empty() {
                            view.name.update(cx, |state, cx| {
                                state.set_value(file_name(&first), window, cx)
                            });
                        }
                        cx.notify();
                    })
                });
            }
        })
        .detach();
    }
    fn drafts(&self, cx: &App) -> Result<Vec<Source>, String> {
        if self.kind == Kind::File {
            if self.files.is_empty() {
                return Err("Choose a location first.".into());
            }
            let sources = self
                .files
                .iter()
                .map(|file| Source {
                    id: self
                        .source
                        .as_ref()
                        .map_or_else(|| file.id.clone(), |s| s.id.clone()),
                    name: file.name.read(cx).value().trim().to_string(),
                    kind: SourceKind::File {
                        path: file.path.clone(),
                    },
                    enabled: true,
                    excluded: self
                        .source
                        .as_ref()
                        .map(|s| s.excluded.clone())
                        .unwrap_or_default(),
                })
                .collect::<Vec<_>>();
            Preferences {
                system_enabled: false,
                sources: sources.clone(),
            }
            .validate()?;
            return Ok(sources);
        }
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
        let sources = vec![Source {
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
                    .child(crate::i18n::text(label)),
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
                Button::new(SharedString::from(format!("source-type-{label}"))).small().outline().icon(icon).label(crate::i18n::text(label)).selected(self.kind == kind).disabled(self.busy || self.source.is_some() && self.kind != kind)
                    .on_click(cx.listener(move |view, _, window, cx| {
                        if view.kind == kind { return; }
                        view.kind = kind;
                        view.manual_path = false;
                        view.path.update(cx, |state, cx| state.set_value("", window, cx));
                        view.invalidate(cx);
                        if kind != Kind::File { view.name.read(cx).focus_handle(cx).focus(window, cx); }
                    }))
            })))
            .when(self.kind != Kind::File, |v| v.child(name))
            .when(self.kind == Kind::File, |v| v.child(self.file_form(cx)))
            .when(self.kind == Kind::Directory, |v| v.child(v_flex().gap_2()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(crate::i18n::text("Folder")))
                .child(h_flex().gap_2().items_center().child(div().flex_1().min_w_0().child(Input::new(&self.path).small()))
                    .child(Button::new("browse-kubeconfig").small().outline().label(crate::i18n::text("Browse…")).on_click(cx.listener(|view, _, window, cx| view.choose(false, window, cx)))))))
            .when(self.kind == Kind::Directory, |v| v.child(Self::field("File names", &self.patterns))
                .child(checkbox("recursive-kubeconfig-scan").label(crate::i18n::text("Include subfolders")).checked(self.recursive).on_click(cx.listener(|view, value, _, cx| { view.recursive = *value; cx.notify(); }))))
            .when(self.kind == Kind::Paste, |v| v.child(v_flex().gap_2().child(div().text_sm().font_weight(FontWeight::MEDIUM).child(crate::i18n::text("Kubeconfig YAML")))
                .child(div().h(px(190.)).border_1().border_color(cx.theme().border).rounded(cx.theme().radius).child(Editor::new(&self.yaml).h_full())))
                .child(Self::field("Base folder (only for relative file references)", &self.base))
                .child(Button::new("browse-kubeconfig-base").small().ghost().icon(IconName::FolderOpen).label(crate::i18n::text("Choose base folder…")).on_click(cx.listener(|view, _, window, cx| view.choose(true, window, cx)))))
            .when(self.kind != Kind::File, |v| v.child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text(match self.kind { Kind::Directory => "Only matching kubeconfig files are loaded. Invalid files do not block the rest of the folder.", _ => "Beacon keeps an owner-only copy locally. Credentials and exec helpers are not used during this preview." }))))
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
        })).child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text("Contexts with the same name stay separate. Adding a source does not connect a cluster; exec credential helpers run only when you connect.")))
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
                    .child(crate::i18n::text(if self.rename {
                        "Changes apply to the source label in all windows."
                    } else if reviewed {
                        "2 of 2 · Choose which contexts appear in Beacon"
                    } else {
                        "1 of 2 · Choose a source"
                    })),
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
                            .label(crate::i18n::text(if reviewed { "Back" } else { "Cancel" }))
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
                            .label(crate::i18n::text(if self.busy {
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
                            }))
                            .disabled(
                                self.busy
                                    || !self.rename
                                        && !reviewed
                                        && self.kind == Kind::File
                                        && self.files.is_empty()
                                    || reviewed && count == 0 && self.source.is_none(),
                            )
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
                    view.add_files(vec![path.to_owned()], window, cx);
                    view.files[0]
                        .name
                        .update(cx, |state, cx| state.set_value("Team", window, cx));
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
    fn file_rows_append_deduplicate_rename_and_remove_before_review() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let paths: Vec<_> = ["dev.yaml", "prod.yaml", "shared.yaml"]
            .map(|name| directory.path().join(name))
            .into_iter()
            .collect();
        for path in &paths {
            std::fs::write(path, yaml()).unwrap();
        }
        let (window, editor) = editor(cx, &paths[0]);
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |view, cx| {
                view.add_files(
                    vec![paths[1].clone(), paths[0].clone(), paths[2].clone()],
                    window,
                    cx,
                )
            });
            window.render_frame(cx);
            let files = &editor.read(cx).files;
            assert_eq!(files.len(), 3);
            let ids: Vec<_> = files.iter().map(|file| file.name.entity_id()).collect();
            let names: Vec<_> = ids
                .iter()
                .map(|id| window.find(("source-file-name", *id)).bounds())
                .collect();
            assert!(
                names
                    .iter()
                    .all(|bounds| bounds.origin.x == names[0].origin.x
                        && bounds.size.width == names[0].size.width)
            );
            window.click(("source-file-name", ids[1]), cx);
            window.press("cmd-a", cx);
            window.input("Production", cx);
            window.click(("remove-source-file", ids[0]), cx);
            assert!(window.try_find(("source-file-row", ids[0])).is_none());
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            assert_eq!(
                editor
                    .read(cx)
                    .drafts(cx)
                    .unwrap()
                    .iter()
                    .map(|source| source.name.as_str())
                    .collect::<Vec<_>>(),
                ["Production", "shared"]
            );
            window.click("confirm-source-editor", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            editor.read_with(cx, |view, _| view.review.is_some())
        });
        editor.read_with(cx, |view, _| {
            let review = view.review.as_ref().unwrap();
            assert_eq!(review.sources.len(), 2);
            assert_eq!(review.sources[0].name, "Production");
            assert_eq!(
                review.sources[0].kind,
                SourceKind::File {
                    path: paths[1].clone()
                }
            );
        });
        cx.update_window(window, |_, window, cx| {
            window.click("cancel-source-editor", cx);
            assert_eq!(editor.read(cx).files[0].name.read(cx).value(), "Production");
        })
        .unwrap();
        assert!(cx.read(|cx| {
            settings::store(cx)
                .read(cx)
                .preferences
                .kubeconfigs
                .sources
                .is_empty()
        }));
        for path in &paths {
            assert_eq!(std::fs::read_to_string(path).unwrap(), yaml());
        }
    }

    #[::core::prelude::v1::test]
    fn empty_file_selection_supports_manual_paths_and_keeps_folder_drafts_separate() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let path = directory.path().join("team config.yaml");
        let (window, editor) = editor(cx, &path);
        cx.update_window(window, |_, window, cx| {
            let id = editor.read(cx).files[0].name.entity_id();
            window.render_frame(cx);
            window.click(("remove-source-file", id), cx);
            assert!(window.find("source-files-empty").visible());
            window.click("confirm-source-editor", cx);
            assert!(editor.read(cx).review.is_none());
            window.click("enter-kubeconfig-path", cx);
            window.input(&path.display().to_string(), cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            let view = editor.read(cx);
            assert!(!view.manual_path);
            assert_eq!(view.files.len(), 1);
            assert_eq!(view.files[0].path, path);
            assert_eq!(view.files[0].name.read(cx).value(), "team config");
            assert!(view.review.is_none());
            window.click(SharedString::from("source-type-Folder"), cx);
            editor.update(cx, |view, cx| {
                view.path.update(cx, |input, cx| {
                    input.set_value(directory.path().display().to_string(), window, cx)
                })
            });
        })
        .unwrap();
        cx.run_until_parked();
        editor.read_with(cx, |view, cx| {
            let drafts = view.drafts(cx).unwrap();
            assert_eq!(drafts.len(), 1);
            assert!(matches!(drafts[0].kind, SourceKind::Directory { .. }));
        });
        cx.update_window(window, |_, window, cx| {
            window.click(SharedString::from("source-type-File"), cx);
            assert_eq!(editor.read(cx).files[0].path, path);
            assert!(window.find("source-file-list").visible());
        })
        .unwrap();
    }

    #[::core::prelude::v1::test]
    fn many_file_rows_scroll_inside_a_bounded_list_and_replacement_keeps_identity() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let (window, editor) = editor(cx, &directory.path().join("first.yaml"));
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |view, cx| {
                view.add_files(
                    (1..12)
                        .map(|n| directory.path().join(format!("team-{n}.yaml")))
                        .collect(),
                    window,
                    cx,
                )
            });
            window.render_frame(cx);
            let id = editor.read(cx).files[0].name.entity_id();
            let initial = window.find(("source-file-row", id)).bounds();
            let footer = window.find("confirm-source-editor").bounds();
            assert!(window.find("source-file-list").bounds().size.height <= px(264.));
            window.scroll(
                "source-file-list",
                ScrollDelta::Pixels(point(px(0.), px(-150.))),
                cx,
            );
            assert!(window.find(("source-file-row", id)).bounds().origin.y < initial.origin.y);
            assert_eq!(window.find("confirm-source-editor").bounds(), footer);

            let source = Source {
                id: "saved-file".into(),
                name: "Team alias".into(),
                kind: SourceKind::File {
                    path: directory.path().join("old.yaml"),
                },
                enabled: false,
                excluded: ["excluded-context".into()].into_iter().collect(),
            };
            let edit = cx.new(|cx| SourceEditor::new(Some(source.clone()), false, window, cx));
            edit.update(cx, |view, cx| {
                view.add_files(
                    vec![
                        directory.path().join("replacement.yaml"),
                        directory.path().join("ignored.yaml"),
                    ],
                    window,
                    cx,
                );
                let drafts = view.drafts(cx).unwrap();
                assert_eq!(drafts.len(), 1);
                assert_eq!(drafts[0].id, source.id);
                assert_eq!(drafts[0].name, source.name);
                assert_eq!(drafts[0].excluded, source.excluded);
                assert_eq!(
                    drafts[0].kind,
                    SourceKind::File {
                        path: directory.path().join("replacement.yaml")
                    }
                );
            });
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
