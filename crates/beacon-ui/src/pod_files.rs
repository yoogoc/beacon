//! A standalone, movable workspace tab for a pinned Pod/container filesystem.
use crate::{bridge::Bridge, copyable_text::copyable_text, yaml_review};
use beacon_kube::{
    ClusterSession, DynamicObject,
    files::{self, Browser, Entry, Listing, Preview, Target},
};
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectGroup, SelectState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

pub(crate) struct FilesRequested {
    pub session: Arc<ClusterSession>,
    pub object: Arc<DynamicObject>,
}

type ContainerChoices = SearchableVec<SelectGroup<String>>;

pub(crate) struct FilesView {
    pub session: Arc<ClusterSession>,
    pub object: Arc<DynamicObject>,
    containers: Vec<files::Container>,
    picker: Entity<SelectState<ContainerChoices>>,
    path: Entity<InputState>,
    search: Entity<InputState>,
    split: Entity<ResizableState>,
    listing: Option<Arc<Listing>>,
    visible: Arc<Vec<usize>>,
    preview: Option<Preview>,
    editor: Option<Entity<TextareaState>>,
    selected: BTreeSet<String>,
    busy: bool,
    message: Option<String>,
    progress: Arc<AtomicU64>,
    transfer: bool,
    writing: bool,
    abort: Option<tokio::task::AbortHandle>,
    _task: Option<Task<()>>,
    _progress: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    _review: Option<Subscription>,
    _prompt: Option<Subscription>,
    _editor_changes: Option<Subscription>,
    window: AnyWindowHandle,
    initial_file: Option<String>,
}
impl Drop for FilesView {
    fn drop(&mut self) {
        if let Some(abort) = &self.abort {
            abort.abort();
        }
    }
}

#[derive(Clone)]
enum Work {
    List(String),
    Preview(String),
    Save(Preview, String),
    Mkdir(String, String),
    Rename(String, String),
    Delete(Vec<String>),
    Upload(PathBuf, String, bool),
    Download(Vec<String>, bool, PathBuf),
}
enum Done {
    List(Listing),
    Preview(Preview),
    Changed,
    Transferred(String),
    Uploaded(String),
}
impl FilesView {
    pub fn new(
        session: Arc<ClusterSession>,
        object: Arc<DynamicObject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let containers = files::containers(&object);
        let names = containers
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>();
        let preferred = object
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get("kubectl.kubernetes.io/default-container"))
            .filter(|name| names.contains(name))
            .cloned()
            .or_else(|| names.first().cloned());
        let picker = cx.new(|cx| {
            let mut picker = SelectState::new(
                SearchableVec::new(
                    ["Containers", "Init containers", "Debug containers"]
                        .into_iter()
                        .filter_map(|group| {
                            let names = containers
                                .iter()
                                .filter(|c| c.group == group)
                                .map(|c| c.name.clone())
                                .collect::<Vec<_>>();
                            (!names.is_empty()).then(|| SelectGroup::new(group).items(names))
                        })
                        .collect::<Vec<_>>(),
                ),
                None,
                window,
                cx,
            )
            .searchable(true);
            if let Some(name) = &preferred {
                picker.set_selected_value(name, window, cx);
            }
            picker
        });
        let path = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("/")
                .placeholder("Absolute container path")
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files"));
        let mut view = Self {
            session,
            object,
            containers,
            picker,
            path,
            search,
            split: cx.new(|_| ResizableState::default()),
            listing: None,
            visible: Arc::new(Vec::new()),
            preview: None,
            editor: None,
            selected: BTreeSet::new(),
            busy: false,
            message: None,
            progress: Arc::new(AtomicU64::new(0)),
            transfer: false,
            writing: false,
            abort: None,
            _task: None,
            _progress: None,
            _subscriptions: Vec::new(),
            _review: None,
            _prompt: None,
            _editor_changes: None,
            window: window.window_handle(),
            initial_file: None,
        };
        view.bind_window(window, cx);
        if preferred.is_some() {
            view.load("/".into(), window, cx);
        } else {
            view.message = Some("No running containers. Files requires a running container with sh and standard file utilities; distroless images may need a debug container.".into());
        }
        view
    }
    pub fn bind_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.window != window.window_handle() {
            let old = self.window;
            let dialog = self._review.take().is_some() || self._prompt.take().is_some();
            if dialog {
                let _ = old.update(cx, |_, window, cx| window.close_dialog(cx));
            }
            // A native chooser belongs to its originating window.
            if self.busy && self.abort.is_none() {
                self.cancel(cx);
            }
            self.window = window.window_handle();
        }
        self._subscriptions = vec![
            cx.subscribe_in(
                &self.picker,
                window,
                |view, _, event: &SelectEvent<ContainerChoices>, window, cx| {
                    if matches!(event, SelectEvent::Confirm(Some(_))) {
                        if view.dirty(cx) {
                            view.message = Some(
                                "Save or discard the current edit before switching containers."
                                    .into(),
                            );
                            cx.notify();
                            return;
                        }
                        view.cancel(cx);
                        view.listing = None;
                        view.preview = None;
                        view.selected.clear();
                        view.load("/".into(), window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &self.path,
                window,
                |view, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        view.load(view.path.read(cx).value().to_string(), window, cx);
                    }
                },
            ),
            cx.subscribe(&self.search, |view, _, _: &InputEvent, cx| {
                view.refilter(cx);
                cx.notify();
            }),
        ];
    }
    pub fn navigation(&self, cx: &App) -> (Option<String>, String, Option<String>) {
        (
            self.picker.read(cx).selected_value().cloned(),
            self.listing
                .as_ref()
                .map_or_else(|| "/".into(), |l| l.path.clone()),
            self.preview.as_ref().map(|p| p.entry.path.clone()),
        )
    }
    pub fn restore_navigation(
        &mut self,
        container: Option<String>,
        directory: String,
        file: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel(cx);
        if let Some(container) = container {
            self.picker
                .update(cx, |p, cx| p.set_selected_value(&container, window, cx));
        }
        self.initial_file = file;
        self.load(directory, window, cx);
    }
    pub fn reopen(
        &mut self,
        object: Arc<DynamicObject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.containers != files::containers(&object) {
            if self.pending(cx).is_some() {
                self.message=Some("Save or discard your edit and cancel any transfer before reopening the current containers.".into());
                cx.notify();
                return;
            }
            self.cancel(cx);
            *self = Self::new(self.session.clone(), object, window, cx);
        } else {
            self.object = object;
        }
        cx.notify();
    }
    pub fn focus_editor(&self, window: &mut Window, cx: &mut App) {
        if let Some(editor) = &self.editor {
            editor.read(cx).focus_handle(cx).focus(window, cx);
        }
    }
    pub fn title(&self) -> SharedString {
        format!(
            "Files · {}",
            self.object.metadata.name.as_deref().unwrap_or("Pod")
        )
        .into()
    }
    pub fn dirty(&self, cx: &App) -> bool {
        self.editor
            .as_ref()
            .zip(self.preview.as_ref())
            .is_some_and(|(editor, p)| p.text.as_deref() != Some(editor.read(cx).value().as_str()))
    }
    pub fn pending(&self, cx: &App) -> Option<String> {
        (self.dirty(cx) || self.busy)
            .then(|| format!("{}: unsaved file or active file operation", self.title()))
    }
    fn browser(&self, cx: &App) -> Option<Browser> {
        let name = self.picker.read(cx).selected_value()?;
        let container = self.containers.iter().find(|c| &c.name == name)?;
        Some(Browser {
            session: self.session.clone(),
            target: Target {
                namespace: self.object.metadata.namespace.clone()?,
                pod: self.object.metadata.name.clone()?,
                uid: self.object.metadata.uid.clone()?,
                container: container.name.clone(),
                container_id: container.id.clone(),
            },
        })
    }
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.cancel(cx);
    }
    pub fn close_dialogs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let review = self._review.take().is_some();
        let prompt = self._prompt.take().is_some();
        if review || prompt {
            window.close_dialog(cx);
        }
    }
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
        self._task = None;
        self._progress = None;
        self.busy = false;
        self.transfer = false;
        self.writing = false;
        self.message = Some("Operation cancelled. Refresh to check the current directory.".into());
        cx.notify();
    }
    fn load(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty(cx) {
            self.message = Some("Save or discard your changes before navigating.".into());
            cx.notify();
            return;
        }
        self.start(Work::List(path), window, cx);
    }
    fn start(&mut self, work: Work, _window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(browser) = self.browser(cx) else {
            self.message = Some("Select a running container.".into());
            cx.notify();
            return;
        };
        self._review = None;
        self._prompt = None;
        self.busy = true;
        self.message = None;
        self.transfer = matches!(work, Work::Upload(..) | Work::Download(..));
        self.writing = !matches!(work, Work::List(_) | Work::Preview(_) | Work::Download(..));
        self.progress = Arc::new(AtomicU64::new(0));
        let progress = self.progress.clone();
        let task = Bridge::global(cx).run_cancellable(async move {
            Ok::<Done, String>(match work {
                Work::List(path) => Done::List(browser.list(&path).await?),
                Work::Preview(path) => Done::Preview(browser.preview(&path).await?),
                Work::Save(preview, text) => {
                    browser.save(&preview, text).await?;
                    Done::Preview(browser.preview(&preview.entry.path).await?)
                }
                Work::Mkdir(dir, name) => {
                    browser.mkdir(&dir, &name).await?;
                    Done::Changed
                }
                Work::Rename(path, name) => {
                    browser.rename(&path, &name).await?;
                    Done::Changed
                }
                Work::Delete(paths) => {
                    browser.delete(&paths).await?;
                    Done::Changed
                }
                Work::Upload(local, path, overwrite) => {
                    browser.upload(local, &path, overwrite, progress).await?;
                    Done::Uploaded(format!("Uploaded to {path}"))
                }
                Work::Download(paths, archive, local) => {
                    let count = browser
                        .download(&paths, archive, local.clone(), progress)
                        .await?;
                    Done::Transferred(format!("Saved {count} bytes to {}", local.display()))
                }
            })
        });
        self.abort = Some(task.abort_handle());
        if self.transfer {
            self._progress = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(150))
                        .await;
                    if this
                        .update(cx, |view, cx| {
                            if view.transfer {
                                cx.notify();
                            }
                            view.transfer
                        })
                        .ok()
                        != Some(true)
                    {
                        break;
                    }
                }
            }));
        }
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = task
                .result()
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            if let Ok(handle) = this.update(cx, |view, _| view.window) {
                let _ = cx.update_window(handle, |_, window, cx| {
                    let _ = this.update(cx, |view, cx| view.completed(result, window, cx));
                });
            }
        }));
        cx.notify();
    }
    fn completed(
        &mut self,
        result: Result<Done, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        self.transfer = false;
        self.writing = false;
        self.abort = None;
        self._progress = None;
        match result {
            Ok(Done::List(listing)) => {
                self.path.update(cx, |input, cx| {
                    input.set_value(listing.path.clone(), window, cx)
                });
                self.listing = Some(Arc::new(listing));
                self.refilter(cx);
                self.preview = None;
                self.editor = None;
                self.selected.clear();
                if let Some(file) = self.initial_file.take() {
                    self.start(Work::Preview(file), window, cx);
                }
            }
            Ok(Done::Preview(preview)) => {
                if preview.entry.directory() {
                    self.load(preview.entry.path, window, cx);
                } else {
                    self.preview = Some(preview);
                    self.editor = None;
                }
            }
            Ok(Done::Changed) => {
                self.preview = None;
                self.editor = None;
                self.selected.clear();
                if let Some(list) = &self.listing {
                    self.load(list.path.clone(), window, cx);
                }
            }
            Ok(Done::Uploaded(message)) => {
                self.preview = None;
                self.editor = None;
                self.selected.clear();
                if let Some(list) = &self.listing {
                    self.load(list.path.clone(), window, cx);
                }
                self.message = Some(message);
            }
            Ok(Done::Transferred(message)) => {
                self.message = Some(message);
            }
            Err(error) => self.message = Some(error),
        }
        cx.notify();
    }
    fn open_entry(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty(cx) {
            self.message = Some("Save or discard the current edit first.".into());
            cx.notify();
            return;
        }
        if entry.directory() {
            self.load(entry.path, window, cx);
        } else {
            self.start(Work::Preview(entry.path), window, cx);
        }
    }
    fn edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = &self.preview
            && p.editable()
        {
            self.editor = Some(cx.new(|cx| {
                let mut editor = TextareaState::new(window, cx).auto_grow(15, 30);
                editor.set_value(p.text.clone().unwrap(), window, cx);
                editor
            }));
            if let Some(editor) = &self.editor {
                self._editor_changes = Some(cx.observe(editor, |_, _, cx| cx.notify()));
            }
            cx.notify();
        }
    }
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = self.preview.clone() else {
            return;
        };
        let Some(editor) = &self.editor else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        if !self.dirty(cx) || self.busy {
            return;
        }
        match yaml_review::Preview::text(preview.text.as_deref().unwrap_or(""), text) {
            Ok(diff) => {
                let context = format!(
                    "{} · {} · {} · {}",
                    self.session.id(),
                    self.object.metadata.namespace.as_deref().unwrap_or(""),
                    self.browser(cx)
                        .map(|b| b.target.container)
                        .unwrap_or_default(),
                    preview.entry.path
                );
                let review = yaml_review::open_text(diff, context, window, cx);
                self._review = Some(cx.subscribe_in(
                    &review,
                    window,
                    move |view, _, event: &yaml_review::ReviewEvent, window, cx| {
                        if let yaml_review::ReviewEvent::Confirmed(value) = event
                            && let Some(text) = value.as_str()
                        {
                            view.start(Work::Save(preview.clone(), text.into()), window, cx);
                        }
                    },
                ));
            }
            Err(error) => {
                self.message = Some(error);
                cx.notify();
            }
        }
    }
    fn targets(&self) -> Vec<Entry> {
        let mut entries = self
            .listing
            .as_ref()
            .map(|l| {
                l.entries
                    .iter()
                    .filter(|e| self.selected.contains(&e.path))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if entries.is_empty()
            && let Some(preview) = &self.preview
        {
            entries.push(preview.entry.clone());
        }
        entries
    }
    #[allow(clippy::too_many_arguments)]
    fn ask(
        &mut self,
        title: &str,
        label: &str,
        initial: Option<String>,
        entries: Vec<Entry>,
        work: impl Fn(String) -> Work + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.dirty(cx) {
            self.message = Some("Finish the current operation or discard your edit first.".into());
            cx.notify();
            return;
        }
        let prompt = FilePrompt::open(title, label, initial, entries, window, cx);
        self._prompt = Some(cx.subscribe_in(
            &prompt,
            window,
            move |view, _, event: &FileConfirmed, window, cx| {
                view.start(work(event.0.clone()), window, cx)
            },
        ));
    }
    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.targets();
        if entries.is_empty() {
            return;
        }
        let paths = entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>();
        self.ask(
            "Delete selected files?",
            "Delete permanently",
            None,
            entries,
            move |_| Work::Delete(paths.clone()),
            window,
            cx,
        );
    }
    fn mkdir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(list) = &self.listing else {
            return;
        };
        let dir = list.path.clone();
        self.ask(
            "New directory",
            "Create directory",
            Some(String::new()),
            Vec::new(),
            move |name| Work::Mkdir(dir.clone(), name),
            window,
            cx,
        );
    }
    fn rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.targets();
        if entries.len() != 1 {
            return;
        }
        let entry = entries[0].clone();
        let path = entry.path.clone();
        self.ask(
            "Rename",
            "Rename",
            Some(entry.name().into()),
            entries,
            move |name| Work::Rename(path.clone(), name),
            window,
            cx,
        );
    }
    fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(list) = &self.listing else {
            return;
        };
        let directory = list.path.clone();
        if self.busy || self.dirty(cx) || !list.writable {
            return;
        }
        let choosing = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a file to upload".into()),
        });
        self.busy = true;
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let choice = choosing.await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.busy = false;
                match choice {
                    Ok(Ok(Some(paths))) if !paths.is_empty() => {
                        let local = paths[0].clone();
                        let destination = local
                            .file_name()
                            .and_then(|name| name.to_str())
                            .ok_or_else(|| "Use a UTF-8 filename".to_string())
                            .and_then(|name| files::child(&directory, name));
                        match destination {
                            Ok(destination) => {
                                let existing = view
                                    .listing
                                    .as_ref()
                                    .and_then(|l| l.entries.iter().find(|e| e.path == destination))
                                    .cloned();
                                if let Some(existing) = existing {
                                    view.ask(
                                        "Overwrite container file?",
                                        "Overwrite",
                                        None,
                                        vec![existing],
                                        move |_| {
                                            Work::Upload(local.clone(), destination.clone(), true)
                                        },
                                        window,
                                        cx,
                                    );
                                } else {
                                    view.start(Work::Upload(local, destination, false), window, cx);
                                }
                            }
                            Err(e) => view.message = Some(e),
                        }
                    }
                    Ok(Ok(_)) => {}
                    _ => view.message = Some("Could not open the local file chooser.".into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.targets();
        if entries.is_empty() || self.busy {
            return;
        }
        let archive = entries.len() > 1 || !entries[0].regular() || entries[0].link.is_some();
        if archive && !self.listing.as_ref().is_some_and(|l| l.tar) {
            self.message =
                Some("Directory and multi-file downloads require tar in the container.".into());
            cx.notify();
            return;
        }
        let paths = entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>();
        let name = if archive {
            format!(
                "{}-files.tar",
                self.object.metadata.name.as_deref().unwrap_or("pod")
            )
        } else {
            files::local_filename(entries[0].name())
        };
        let choosing = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a download folder".into()),
        });
        self.busy = true;
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let choice = choosing.await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.busy = false;
                match choice {
                    Ok(Ok(Some(folders))) if !folders.is_empty() => view.start(
                        Work::Download(paths, archive, folders[0].join(name)),
                        window,
                        cx,
                    ),
                    Ok(Ok(_)) => {}
                    _ => view.message = Some("Could not open the folder chooser.".into()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn refilter(&mut self, cx: &App) {
        let query = self.search.read(cx).value().to_lowercase();
        self.visible = Arc::new(
            self.listing
                .as_ref()
                .map(|l| {
                    l.entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| e.name().to_lowercase().contains(&query))
                        .map(|(i, _)| i)
                        .collect()
                })
                .unwrap_or_default(),
        );
    }
    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let listing = self.listing.clone();
        let visible = self.visible.clone();
        let count = visible.len();
        let this = cx.entity().downgrade();
        let selected = self.selected.clone();
        let focused = self.preview.as_ref().map(|p| p.entry.path.clone());
        let list = uniform_list("pod-file-list", count, move |range, _, cx| {
            range
                .map(|index| {
                    let entry = &listing.as_ref().unwrap().entries[visible[index]];
                    let key = entry.path.clone();
                    let checked = selected.contains(&key);
                    let open = this.clone();
                    let pick = this.clone();
                    let target = entry.clone();
                    let menu = this.clone();
                    let menu_entry = entry.clone();
                    let hover = format!(
                        "{}\n{} · {} · mode {} · owner {}\nModified {}",
                        entry.path,
                        entry.kind,
                        size(entry.size),
                        entry.permissions,
                        entry.owner,
                        modified(entry.modified)
                    );
                    h_flex()
                        .id(SharedString::from(format!("file-row-{index}")))
                        .h(px(34.))
                        .w_full()
                        .gap_2()
                        .px_2()
                        .text_sm()
                        .cursor_pointer()
                        .tooltip(move |w, cx| Tooltip::new(hover.clone()).build(w, cx))
                        .when(focused.as_ref() == Some(&key) || checked, |row| {
                            row.bg(cx.theme().muted)
                        })
                        .hover(|row| row.bg(cx.theme().muted.opacity(0.6)))
                        .child(
                            Checkbox::new(SharedString::from(format!("select-file-{index}")))
                                .checked(checked)
                                .accessibility_label(format!("Select {}", entry.name()))
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    let _ = pick.update(cx, |view, cx| {
                                        if !view.selected.insert(key.clone()) {
                                            view.selected.remove(&key);
                                        }
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            div().flex_1().min_w_0().overflow_hidden().child(
                                Button::new(SharedString::from(format!("open-file-{index}")))
                                    .xsmall()
                                    .ghost()
                                    .icon(if entry.directory() {
                                        IconName::Folder
                                    } else if entry.link.is_some() {
                                        IconName::Link
                                    } else {
                                        IconName::File
                                    })
                                    .label(entry.name().to_owned())
                                    .tooltip(entry.path.clone())
                                    .on_click({
                                        let open = open.clone();
                                        let target = target.clone();
                                        move |_, w, cx| {
                                            cx.stop_propagation();
                                            let _ = open.update(cx, |v, cx| {
                                                v.open_entry(target.clone(), w, cx)
                                            });
                                        }
                                    }),
                            ),
                        )
                        .child(div().w(px(75.)).text_xs().child(size(entry.size)))
                        .child(div().w(px(55.)).text_xs().child(entry.permissions.clone()))
                        .on_click(move |_, window, cx| {
                            let _ = open
                                .update(cx, |view, cx| view.open_entry(target.clone(), window, cx));
                        })
                        .context_menu(move |popup, _, cx| {
                            let open = menu.clone();
                            let target = menu_entry.clone();
                            let copy = menu_entry.path.clone();
                            let download = menu.clone();
                            let delete = menu.clone();
                            let for_download = menu_entry.clone();
                            let for_delete = menu_entry.clone();
                            let writable = menu.upgrade().is_some_and(|v| {
                                v.read(cx).listing.as_ref().is_some_and(|l| l.writable)
                            }) && menu_entry.writable;
                            popup
                                .item(PopupMenuItem::new("Open").on_click(move |_, window, cx| {
                                    let _ = open.update(cx, |v, cx| {
                                        v.open_entry(target.clone(), window, cx)
                                    });
                                }))
                                .item(PopupMenuItem::new("Download…").on_click(
                                    move |_, window, cx| {
                                        let _ = download.update(cx, |v, cx| {
                                            v.selected =
                                                BTreeSet::from([for_download.path.clone()]);
                                            v.download(window, cx);
                                        });
                                    },
                                ))
                                .item(crate::copyable_text::copy_item("Copy path", copy))
                                .separator()
                                .item(PopupMenuItem::new("Delete…").disabled(!writable).on_click(
                                    move |_, window, cx| {
                                        let _ = delete.update(cx, |v, cx| {
                                            if !v.selected.contains(&for_delete.path) {
                                                v.selected =
                                                    BTreeSet::from([for_delete.path.clone()]);
                                            }
                                            v.delete(window, cx);
                                        });
                                    },
                                ))
                        })
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        })
        .size_full();
        v_flex()
            .size_full()
            .gap_2()
            .p_2()
            .child(Input::new(&self.search).id("files-search").xsmall())
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().flex_1().child("Name"))
                    .child(div().w(px(75.)).child("Size"))
                    .child(div().w(px(55.)).child("Mode")),
            )
            .child(div().flex_1().min_h_0().child(list))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{count} items · {} selected", self.selected.len())),
            )
            .into_any_element()
    }
    fn render_preview(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(preview) = &self.preview else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("Select a file to preview")
                .into_any_element();
        };
        let entry = &preview.entry;
        let mut body = v_flex().size_full().gap_3().p_3()
            .child(copyable_text("file-path",entry.path.clone()))
            .child(h_flex().w_full().gap_2()
                .child(Button::new("edit-file").xsmall().ghost().label("Edit").disabled(!preview.editable() || self.busy || self.editor.is_some()).on_click(cx.listener(|v,_,w,cx| v.edit(w,cx))))
                .when(self.editor.is_some(),|bar| bar.child(Button::new("save-file").xsmall().primary().label("Review & save").disabled(self.busy || !self.dirty(cx)).on_click(cx.listener(|v,_,w,cx| v.save(w,cx))))
                    .child(Button::new("discard-file").xsmall().ghost().label("Discard").disabled(self.busy).on_click(cx.listener(|v,_,_,cx| { v.editor = None; cx.notify(); }))))
                .child(Button::new("download-preview").xsmall().ghost().label("Download…").disabled(self.busy).on_click(cx.listener(|v,_,w,cx| { v.selected.clear(); v.download(w,cx); }))))
            .child(copyable_text("file-metadata",format!("{} · {} · mode {} · UID:GID {} · modified {}",entry.kind,size(entry.size),entry.permissions,entry.owner,modified(entry.modified))))
            .when(!entry.writable,|body| body.child(copyable_text("file-readonly","Read-only mount or file permissions. Editing is disabled.")))
            .when(preview.truncated,|body| body.child(copyable_text("file-truncated","Preview limited to 256 KiB. Download for the complete file; truncated previews cannot be edited.")));
        if let Some(link) = &entry.link {
            let target = if link.starts_with('/') {
                files::path(link)
            } else {
                files::path(&format!(
                    "{}/{}",
                    entry.path.rsplit_once('/').map_or("/", |p| p.0),
                    link
                ))
            };
            body = body
                .child(copyable_text(
                    "symlink-target",
                    format!("Symlink target: {link}"),
                ))
                .child(
                    Button::new("open-link-target")
                        .small()
                        .ghost()
                        .label("Open target")
                        .disabled(self.busy || target.is_err())
                        .on_click(cx.listener(move |v, _, w, cx| {
                            if let Ok(path) = &target {
                                v.start(Work::Preview(path.clone()), w, cx);
                            }
                        })),
                );
        }
        let content = if let Some(editor) = &self.editor {
            Textarea::new(editor).readonly(self.busy).into_any_element()
        } else if let Some(text) = &preview.text {
            div()
                .font_family("monospace")
                .whitespace_nowrap()
                .child(copyable_text("file-content", text.clone()))
                .into_any_element()
        } else {
            copyable_text("file-binary","Binary or special file. Text preview is unavailable; download preserves its original bytes.").into_any_element()
        };
        body.child(
            div()
                .id("file-preview-scroll")
                .flex_1()
                .min_h_0()
                .overflow_scroll()
                .child(content),
        )
        .into_any_element()
    }
}
fn modified(seconds: i64) -> String {
    beacon_columns::Timestamp::from_second(seconds)
        .map(|t| t.to_string())
        .unwrap_or_else(|_| seconds.to_string())
}
fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024. * 1024.))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.)
    } else {
        format!("{bytes} B")
    }
}
impl Render for FilesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let writable =
            self.listing.as_ref().is_some_and(|l| l.writable) && !self.busy && !self.dirty(cx);
        let targets = self.targets();
        let may_modify = writable && !targets.is_empty() && targets.iter().all(|e| e.writable);
        let directory = self
            .listing
            .as_ref()
            .map(|l| l.path.clone())
            .unwrap_or_else(|| "/".into());
        let parent = files::path(&format!("{directory}/.."));
        let breadcrumbs = directory
            .split('/')
            .filter(|s| !s.is_empty())
            .scan(String::new(), |path, part| {
                path.push('/');
                path.push_str(part);
                Some((part.to_string(), path.clone()))
            })
            .collect::<Vec<_>>();
        v_flex()
            .size_full()
            .min_size_0()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .text_sm()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(copyable_text(
                        "files-pod",
                        format!(
                            "{} / {}",
                            self.object.metadata.namespace.as_deref().unwrap_or(""),
                            self.object.metadata.name.as_deref().unwrap_or("")
                        ),
                    ))
                    .child(div().flex_1())
                    .child(
                        div().w(px(240.)).child(
                            Select::new(&self.picker)
                                .xsmall()
                                .search_placeholder("Search running containers")
                                .title_prefix("Container: ")
                                .disabled(
                                    self.containers.is_empty() || self.busy || self.dirty(cx),
                                ),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("files-up")
                            .xsmall()
                            .ghost()
                            .label("Up")
                            .disabled(self.busy || self.dirty(cx) || directory == "/")
                            .on_click(cx.listener(move |v, _, w, cx| {
                                if let Ok(path) = &parent {
                                    v.load(path.clone(), w, cx);
                                }
                            })),
                    )
                    .child(div().flex_1().child(Input::new(&self.path).xsmall()))
                    .child(
                        Button::new("files-go")
                            .xsmall()
                            .ghost()
                            .label("Go")
                            .disabled(self.busy || self.dirty(cx))
                            .on_click(cx.listener(|v, _, w, cx| {
                                v.load(v.path.read(cx).value().to_string(), w, cx)
                            })),
                    )
                    .child(
                        Button::new("files-refresh")
                            .xsmall()
                            .ghost()
                            .label("Refresh")
                            .disabled(self.busy || self.dirty(cx))
                            .on_click(cx.listener(|v, _, w, cx| {
                                let path = v
                                    .listing
                                    .as_ref()
                                    .map_or_else(|| "/".into(), |l| l.path.clone());
                                v.load(path, w, cx);
                            })),
                    )
                    .child(
                        Button::new("files-upload")
                            .xsmall()
                            .ghost()
                            .label("Upload…")
                            .disabled(!writable)
                            .on_click(cx.listener(|v, _, w, cx| v.upload(w, cx))),
                    )
                    .child(
                        Button::new("files-download")
                            .xsmall()
                            .ghost()
                            .label("Download…")
                            .disabled(self.busy || targets.is_empty())
                            .on_click(cx.listener(|v, _, w, cx| v.download(w, cx))),
                    )
                    .child(
                        Button::new("files-mkdir")
                            .xsmall()
                            .ghost()
                            .label("New folder")
                            .disabled(!writable)
                            .on_click(cx.listener(|v, _, w, cx| v.mkdir(w, cx))),
                    )
                    .child(
                        Button::new("files-rename")
                            .xsmall()
                            .ghost()
                            .label("Rename")
                            .disabled(!may_modify || targets.len() != 1)
                            .on_click(cx.listener(|v, _, w, cx| v.rename(w, cx))),
                    )
                    .child(
                        Button::new("files-delete")
                            .xsmall()
                            .ghost()
                            .label("Delete…")
                            .disabled(!may_modify)
                            .on_click(cx.listener(|v, _, w, cx| v.delete(w, cx))),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .pb_2()
                    .gap_1()
                    .child(
                        Button::new("files-root")
                            .xsmall()
                            .ghost()
                            .label("/")
                            .disabled(self.busy || self.dirty(cx))
                            .on_click(cx.listener(|v, _, w, cx| v.load("/".into(), w, cx))),
                    )
                    .children(
                        breadcrumbs
                            .into_iter()
                            .enumerate()
                            .map(|(i, (name, path))| {
                                Button::new(("breadcrumb", i))
                                    .xsmall()
                                    .ghost()
                                    .label(name)
                                    .disabled(self.busy || self.dirty(cx))
                                    .on_click(
                                        cx.listener(move |v, _, w, cx| v.load(path.clone(), w, cx)),
                                    )
                            }),
                    ),
            )
            .when(self.busy, |body| {
                body.child(
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .child(copyable_text(
                            "file-progress",
                            if self.transfer {
                                format!(
                                    "Transferring · {}",
                                    size(self.progress.load(Ordering::Relaxed))
                                )
                            } else {
                                "Working…".into()
                            },
                        ))
                        .child(
                            Button::new("files-cancel")
                                .xsmall()
                                .ghost()
                                .label("Cancel")
                                .on_click(cx.listener(|v, _, _, cx| v.cancel(cx))),
                        ),
                )
            })
            .when_some(self.message.clone(), |body, message| {
                body.child(
                    div()
                        .px_3()
                        .py_1()
                        .child(copyable_text("files-message", message)),
                )
            })
            .child(
                div().flex_1().min_size_0().child(
                    h_resizable("files-split")
                        .with_state(&self.split)
                        .child(
                            resizable_panel()
                                .size(px(440.))
                                .size_range(px(220.)..px(10000.))
                                .child(self.render_list(cx)),
                        )
                        .child(
                            resizable_panel().size_range(px(260.)..px(10000.)).child(
                                div()
                                    .size_full()
                                    .border_l_1()
                                    .border_color(cx.theme().border)
                                    .child(self.render_preview(cx)),
                            ),
                        ),
                ),
            )
    }
}

struct FileConfirmed(String);
struct FilePrompt {
    input: Option<Entity<InputState>>,
    entries: Vec<Entry>,
    label: String,
    resolved: bool,
    error: Option<String>,
}
impl EventEmitter<FileConfirmed> for FilePrompt {}
impl FilePrompt {
    fn open(
        title: &str,
        label: &str,
        initial: Option<String>,
        entries: Vec<Entry>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let view = cx.new(|cx| Self {
            input: initial.map(|value| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(value)
                        .placeholder("Name")
                })
            }),
            entries,
            label: label.into(),
            resolved: false,
            error: None,
        });
        let content = view.clone();
        let title = title.to_owned();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .width(px(760.))
                .child(content.clone())
        });
        view
    }
}
impl Render for FilePrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_3()
            .when_some(self.input.clone(), |body, input| {
                body.child(Input::new(&input))
            })
            .when_some(self.error.clone(), |body, error| {
                body.child(copyable_text("file-name-error", error))
            })
            .when(!self.entries.is_empty(), |body| {
                body.child(
                    v_flex()
                        .id("file-target-table")
                        .w_full()
                        .max_h(px(350.))
                        .overflow_y_scroll()
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .py_2()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .child(div().flex_1().child("Container path"))
                                .child(div().w(px(100.)).child("Type"))
                                .child(div().w(px(90.)).child("Size")),
                        )
                        .children(self.entries.iter().enumerate().map(|(i, e)| {
                            h_flex()
                                .id(SharedString::from(format!("file-target-{i}")))
                                .w_full()
                                .gap_2()
                                .py_2()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .text_sm()
                                .child(
                                    div()
                                        .flex_1()
                                        .child(copyable_text(("delete-path", i), e.path.clone())),
                                )
                                .child(div().w(px(100.)).child(if e.directory() {
                                    "Directory"
                                } else if e.link.is_some() {
                                    "Symlink"
                                } else {
                                    "File"
                                }))
                                .child(div().w(px(90.)).child(size(e.size)))
                                .test_support()
                        })),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("file-prompt-cancel")
                            .small()
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("file-prompt-confirm")
                            .small()
                            .primary()
                            .label(self.label.clone())
                            .on_click(cx.listener(|v, _, window, cx| {
                                let value = v
                                    .input
                                    .as_ref()
                                    .map(|i| i.read(cx).value().to_string())
                                    .unwrap_or_default();
                                if v.resolved {
                                    return;
                                }
                                if v.input.is_some()
                                    && let Err(error) = files::child("/", &value)
                                {
                                    v.error = Some(error);
                                    cx.notify();
                                    return;
                                }
                                v.resolved = true;
                                cx.emit(FileConfirmed(value));
                                window.defer(cx, |w, cx| w.close_dialog(cx));
                            })),
                    ),
            )
    }
}

#[cfg(all(test, feature = "ui-tests", unix))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use beacon_kube::{
        ClusterId,
        test_support::{FileFixture, Fixture},
    };
    use gpui_kit::test::TestWindowExt as _;

    fn setup(
        cx: &mut TestAppContext,
    ) -> (FileFixture, Fixture, AnyWindowHandle, Entity<FilesView>) {
        let files = FileFixture::new();
        std::fs::write(files.root.join("a.txt"), "before\n").unwrap();
        std::fs::write(files.root.join("b.bin"), [0, 255, 128, 1]).unwrap();
        let pod = files.pod();
        let (fixture, session) = cx.read(|cx| Bridge::global(cx).handle()).block_on(async {
            let fixture = Fixture::start(vec![pod.clone()]).await;
            fixture.refuse_exec();
            let session = Arc::new(
                ClusterSession::for_testing(
                    ClusterId::new("file-fixture"),
                    fixture.url.clone(),
                    vec![],
                )
                .with_file_test_program(files.program.clone()),
            );
            (fixture, session)
        });
        let object = Arc::new(serde_json::from_value(pod).unwrap());
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                w.set_view_retention(false);
                cx.new(|cx| FilesView::new(session, object, w, cx))
            })
            .unwrap()
        });
        support::settle(cx, |cx| view.read_with(cx, |v, _| !v.busy));
        view.read_with(cx, |v, _| assert!(v.listing.is_some(), "{:?}", v.message));
        (files, fixture, window, view)
    }
    fn open_first(cx: &mut TestAppContext, window: AnyWindowHandle, view: &Entity<FilesView>) {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("open-file-0", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |v, _| !v.busy));
        view.read_with(cx, |v, _| assert!(v.preview.is_some(), "{:?}", v.message));
    }
    #[::core::prelude::v1::test]
    fn actual_file_editor_requires_diff_and_preserves_cancelled_drafts() {
        let cx = &mut support::context();
        let (files, _fixture, window, view) = setup(cx);
        open_first(cx, window, &view);
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("edit-file", cx);
        })
        .unwrap();
        cx.update_window(window, |_, w, cx| {
            let editor = view.read(cx).editor.clone().unwrap();
            editor.read(cx).focus_handle(cx).focus(w, cx);
            w.render_frame(cx);
            w.press(
                if cfg!(target_os = "macos") {
                    "cmd-a"
                } else {
                    "ctrl-a"
                },
                cx,
            );
            w.input("changed-配置\n", cx);
        })
        .unwrap();
        support::render_until(cx, window, |w| {
            w.try_find("save-file")
                .is_some_and(|b| !b.disabled().unwrap_or(false))
        });
        cx.update_window(window, |_, w, cx| w.click("save-file", cx))
            .unwrap();
        support::render_until(cx, window, |w| {
            w.try_find("confirm-yaml-review")
                .is_some_and(|b| b.visible())
        });
        assert_eq!(
            std::fs::read_to_string(files.root.join("a.txt")).unwrap(),
            "before\n"
        );
        cx.update_window(window, |_, w, cx| w.click("cancel-yaml-review", cx))
            .unwrap();
        cx.run_until_parked();
        view.read_with(cx, |v, cx| assert!(v.dirty(cx)));
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("save-file", cx);
        })
        .unwrap();
        support::render_until(cx, window, |w| {
            w.try_find("confirm-yaml-review")
                .is_some_and(|b| b.visible())
        });
        cx.update_window(window, |_, w, cx| w.click("confirm-yaml-review", cx))
            .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |v, cx| !v.busy && !v.dirty(cx)));
        assert_eq!(
            std::fs::read_to_string(files.root.join("a.txt")).unwrap(),
            "changed-配置\n"
        );
    }
    #[::core::prelude::v1::test]
    fn native_folder_download_and_table_confirmed_multidelete() {
        let cx = &mut support::context();
        let (files, _fixture, window, view) = setup(cx);
        let local = tempfile::tempdir().unwrap();
        open_first(cx, window, &view);
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("files-download", cx);
        })
        .unwrap();
        let folder = local.path().to_owned();
        cx.simulate_path_prompt_response(move |options| {
            assert!(options.directories && !options.files && !options.multiple);
            Some(vec![folder.clone()])
        });
        support::settle(cx, |cx| view.read_with(cx, |v, _| !v.busy));
        assert_eq!(
            std::fs::read_to_string(local.path().join("a.txt")).unwrap(),
            "before\n"
        );
        cx.update_window(window, |_, w, cx| {
            view.update(cx, |v, cx| {
                v.selected = v
                    .listing
                    .as_ref()
                    .unwrap()
                    .entries
                    .iter()
                    .map(|e| e.path.clone())
                    .collect();
                cx.notify();
            });
            w.render_frame(cx);
            w.click("files-delete", cx);
        })
        .unwrap();
        support::render_until(cx, window, |w| {
            w.try_find("file-prompt-confirm")
                .is_some_and(|b| b.visible())
        });
        assert_eq!(files.root.read_dir().unwrap().count(), 2);
        cx.update_window(window, |_, w, _| {
            let first = w.find("file-target-0");
            let second = w.find("file-target-1");
            assert!(first.visible() && second.visible());
            assert_eq!(first.bounds().origin.x, second.bounds().origin.x);
            assert_eq!(first.bounds().size.width, second.bounds().size.width);
            assert!(first.bounds().bottom() <= second.bounds().origin.y);
        })
        .unwrap();
        cx.update_window(window, |_, w, cx| w.click("file-prompt-cancel", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(files.root.read_dir().unwrap().count(), 2);
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("files-delete", cx);
        })
        .unwrap();
        support::render_until(cx, window, |w| {
            w.try_find("file-prompt-confirm")
                .is_some_and(|b| b.visible())
        });
        cx.update_window(window, |_, w, cx| w.click("file-prompt-confirm", cx))
            .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |v, _| {
                !v.busy && v.listing.as_ref().is_some_and(|l| l.entries.is_empty())
            })
        });
        assert_eq!(files.root.read_dir().unwrap().count(), 0);
    }
    #[::core::prelude::v1::test]
    fn native_upload_confirms_overwrite_and_searchable_running_containers() {
        let cx = &mut support::context();
        let (files, _fixture, window, view) = setup(cx);
        view.read_with(cx, |v, _| {
            assert_eq!(v.containers.len(), 2);
            assert!(!v.containers.iter().any(|c| c.name == "finished-init"));
        });
        let local = tempfile::tempdir().unwrap();
        let input = local.path().join("a.txt");
        std::fs::write(&input, "uploaded\n").unwrap();
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("files-upload", cx);
        })
        .unwrap();
        cx.simulate_path_prompt_response(move |options| {
            assert!(options.files && !options.directories);
            Some(vec![input.clone()])
        });
        support::render_until(cx, window, |w| {
            w.try_find("file-prompt-confirm")
                .is_some_and(|b| b.visible())
        });
        assert_eq!(
            std::fs::read_to_string(files.root.join("a.txt")).unwrap(),
            "before\n"
        );
        cx.update_window(window, |_, w, cx| w.click("file-prompt-confirm", cx))
            .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |v, _| !v.busy));
        assert_eq!(
            std::fs::read_to_string(files.root.join("a.txt")).unwrap(),
            "uploaded\n"
        );
    }
}

#[cfg(all(test, feature = "ui-tests", unix))]
mod readonly_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    #[::core::prelude::v1::test]
    fn mounted_and_truncated_text_previews_cannot_be_edited() {
        let cx = &mut support::context();
        let files = beacon_kube::test_support::FileFixture::new();
        std::fs::write(files.root.join("a.txt"), "original").unwrap();
        let mut pod = files.pod();
        pod["spec"]["volumes"] =
            serde_json::json!([{"name":"config","configMap":{"name":"fixture"}}]);
        pod["spec"]["containers"][0]["volumeMounts"] =
            serde_json::json!([{"name":"config","mountPath":files.root.to_str().unwrap()}]);
        let (fixture, session) = cx.read(|cx| Bridge::global(cx).handle()).block_on(async {
            let fixture = beacon_kube::test_support::Fixture::start(vec![pod.clone()]).await;
            fixture.refuse_exec();
            let session = Arc::new(
                ClusterSession::for_testing(
                    beacon_kube::ClusterId::new("readonly-files"),
                    fixture.url.clone(),
                    vec![],
                )
                .with_file_test_program(files.program.clone()),
            );
            (fixture, session)
        });
        let object = Arc::new(serde_json::from_value(pod).unwrap());
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                w.set_view_retention(false);
                cx.new(|cx| FilesView::new(session.clone(), object, w, cx))
            })
            .unwrap()
        });
        support::settle(cx, |cx| view.read_with(cx, |v, _| !v.busy));
        view.read_with(cx, |view, cx| {
            assert_eq!(view.browser(cx).unwrap().target.container, "app");
            assert!(
                !view.listing.as_ref().unwrap().writable,
                "{} / {:?}",
                view.listing.as_ref().unwrap().path,
                fixture.object("Pod", "file-pod")
            );
        });
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            // Legacy GPUI buttons can omit the accessibility disabled flag;
            // verify the interaction itself is inert.
            w.click("files-upload", cx);
            w.click("open-file-0", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |v, _| !v.busy));
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert!(!view.read(cx).preview.as_ref().unwrap().editable());
            w.click("edit-file", cx);
        })
        .unwrap();
        view.read_with(cx, |v, _| assert!(v.editor.is_none() && !v.busy));
        assert_eq!(
            std::fs::read_to_string(files.root.join("a.txt")).unwrap(),
            "original"
        );
        fixture.put(files.pod());
        let large = "配置".repeat(files::PREVIEW_LIMIT / 3);
        std::fs::write(files.root.join("large.txt"), &large).unwrap();
        let browser = Browser {
            session,
            target: Target {
                namespace: "default".into(),
                pod: "file-pod".into(),
                uid: "file-uid".into(),
                container: "app".into(),
                container_id: "fixture://app".into(),
            },
        };
        let preview = cx
            .read(|cx| Bridge::global(cx).handle())
            .block_on(browser.preview(files.root.join("large.txt").to_str().unwrap()))
            .unwrap();
        assert!(preview.truncated && preview.text.is_some() && preview.entry.writable);
        cx.update_window(window, |_, w, cx| {
            view.update(cx, |v, cx| {
                v.preview = Some(preview);
                cx.notify();
            });
            w.render_frame(cx);
            assert!(!view.read(cx).preview.as_ref().unwrap().editable());
            w.click("edit-file", cx);
            assert!(view.read(cx).editor.is_none());
        })
        .unwrap();
    }
}
