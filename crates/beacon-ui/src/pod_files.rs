//! A standalone, movable workspace tab for a pinned Pod/container filesystem.
use crate::scroll::OverflowScrollbars as _;
use crate::selection::checkbox;
use crate::{bridge::Bridge, copyable_text::copyable_text, yaml_review};
use beacon_kube::{
    ClusterSession, DynamicObject,
    files::{self, Browser, Entry, Listing, Preview, Target},
};
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, TextareaState};
use gpui_kit::component::resizable::ResizableState;
use gpui_kit::component::select::{SearchableVec, SelectEvent, SelectGroup, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    cell::Cell,
    collections::BTreeSet,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

mod presentation;

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
    width: Rc<Cell<Pixels>>,
    list_width: Rc<Cell<Pixels>>,
    details: bool,
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
                            (!names.is_empty())
                                .then(|| SelectGroup::new(crate::i18n::text(group)).items(names))
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
        let path = cx
            .new(|cx| crate::i18n::input("Absolute container path", window, cx).default_value("/"));
        let search = cx.new(|cx| crate::i18n::input("Filter files", window, cx));
        let mut view = Self {
            session,
            object,
            containers,
            picker,
            path,
            search,
            split: cx.new(|_| ResizableState::default()),
            width: Rc::new(Cell::new(px(900.))),
            list_width: Rc::new(Cell::new(px(440.))),
            details: false,
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
            "{} · {}",
            crate::i18n::text("Files"),
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
        (self.dirty(cx) || self.busy).then(|| {
            crate::i18n::message(
                "messages.unsaved_file_or_active_file_operation",
                &[("value0", (self.title()).to_string())],
            )
            .to_string()
        })
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
                    Done::Uploaded(
                        crate::i18n::message(
                            "messages.uploaded_to_path",
                            &[("path", (path).to_string())],
                        )
                        .to_string(),
                    )
                }
                Work::Download(paths, archive, local) => {
                    let count = browser
                        .download(&paths, archive, local.clone(), progress)
                        .await?;
                    Done::Transferred(
                        crate::i18n::message(
                            "messages.saved_count_bytes_to",
                            &[
                                ("count", (count).to_string()),
                                ("value0", (local.display()).to_string()),
                            ],
                        )
                        .to_string(),
                    )
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
                cx.new(|cx| crate::i18n::input("Name", window, cx).default_value(value))
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
                .title(crate::i18n::text(&title))
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
                        .overflow_y_scrollbar()
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .py_2()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .child(div().flex_1().child(crate::i18n::text("Container path")))
                                .child(div().w(px(100.)).child(crate::i18n::text("Type")))
                                .child(div().w(px(90.)).child(crate::i18n::text("Size"))),
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
                            .label(crate::i18n::text("Cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("file-prompt-confirm")
                            .small()
                            .primary()
                            .label(crate::i18n::text(&self.label))
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

    struct Pane {
        view: Entity<FilesView>,
        width: Pixels,
    }
    impl Render for Pane {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(div().w(self.width).h_full().child(self.view.clone()))
        }
    }

    fn draw(cx: &mut TestAppContext, window: AnyWindowHandle) {
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap();
        }
    }

    fn setup(
        cx: &mut TestAppContext,
    ) -> (FileFixture, Fixture, AnyWindowHandle, Entity<FilesView>) {
        let (files, fixture, window, view, _) = setup_pane(cx);
        (files, fixture, window, view)
    }
    fn setup_pane(
        cx: &mut TestAppContext,
    ) -> (
        FileFixture,
        Fixture,
        AnyWindowHandle,
        Entity<FilesView>,
        Entity<Pane>,
    ) {
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
        let (window, pane) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: gpui_kit::size(px(1500.), px(800.)),
                    })),
                    ..Default::default()
                },
                cx,
                |w, cx| {
                    w.set_view_retention(false);
                    let view = cx.new(|cx| FilesView::new(session, object, w, cx));
                    cx.new(|_| Pane {
                        view,
                        width: px(1100.),
                    })
                },
            )
            .unwrap()
        });
        let view = pane.read_with(cx, |pane, _| pane.view.clone());
        support::settle(cx, |cx| view.read_with(cx, |v, _| !v.busy));
        view.read_with(cx, |v, _| assert!(v.listing.is_some(), "{:?}", v.message));
        (files, fixture, window, view, pane)
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
    fn file_manager_fits_split_panes_and_preview_can_restore_the_full_list() {
        let cx = &mut support::context();
        let (files, _fixture, window, view, pane) = setup_pane(cx);
        for width in [1200., 900., 700., 620., 520., 519., 420., 280.] {
            pane.update(cx, |pane, cx| {
                pane.width = px(width);
                cx.notify();
            });
            draw(cx, window);
            cx.update_window(window, |_, w, _| {
                let root = w.find("pod-files-view").bounds();
                let bar = w.find("files-toolbar").bounds();
                assert!(bar.size.height <= px(48.));
                assert_eq!(
                    w.find("files-list-pane").bounds().size.width,
                    root.size.width
                );
                let path = w.find("files-path-input").bounds();
                assert!(path.size.width >= px(80.));
                for id in [
                    "files-up",
                    "files-go",
                    "files-refresh",
                    "files-upload",
                    "files-download",
                    "files-mkdir",
                    "files-rename",
                    "files-delete",
                    "files-more",
                ] {
                    if let Some(control) = w.try_find(id) {
                        assert!(
                            bar.contains(&control.bounds().center()),
                            "{id} overflowed at {width}px"
                        );
                        assert!(
                            control.bounds().right() <= bar.right(),
                            "{id} overflowed at {width}px"
                        );
                    }
                }
            })
            .unwrap();
        }
        // Narrow panes retain all operations in a keyboard-accessible menu.
        cx.update_window(window, |_, w, cx| w.click("files-more", cx))
            .unwrap();
        support::render_until(cx, window, |w| {
            gpui_kit::base::test_support::snapshots(w)
                .iter()
                .any(|e| e.role() == Some(Role::MenuItem) && e.label() == Some("New folder"))
        });
        cx.update_window(window, |_, w, cx| {
            let item = gpui_kit::base::test_support::snapshots(w)
                .into_iter()
                .find(|e| e.role() == Some(Role::MenuItem) && e.label() == Some("New folder"))
                .unwrap();
            w.click(item.path().last().unwrap().clone(), cx);
        })
        .unwrap();
        support::render_until(cx, window, |w| {
            w.try_find("file-prompt-confirm")
                .is_some_and(|b| b.visible())
        });
        cx.update_window(window, |_, w, cx| w.click("file-prompt-cancel", cx))
            .unwrap();
        assert_eq!(files.root.read_dir().unwrap().count(), 2);
        pane.update(cx, |pane, cx| {
            pane.width = px(1100.);
            cx.notify();
        });
        draw(cx, window);
        open_first(cx, window, &view);
        draw(cx, window);
        cx.update_window(window, |_, w, cx| {
            let list = w.find("files-list-pane").bounds();
            let preview = w.find("files-preview-pane").bounds();
            assert!(list.right() <= preview.origin.x);
            assert!(list.size.width >= px(300.) && preview.size.width >= px(300.));
            assert!(w.try_find("file-metadata").is_none());
            w.click("files-details-toggle", cx);
            w.render_frame(cx);
            assert!(w.find("file-metadata").visible());
        })
        .unwrap();
        pane.update(cx, |pane, cx| {
            pane.width = px(420.);
            cx.notify();
        });
        draw(cx, window);
        cx.update_window(window, |_, w, cx| {
            assert!(w.try_find("files-list-pane").is_none());
            assert_eq!(w.find("files-preview-pane").bounds().size.width, px(420.));
            w.click("files-close-preview", cx);
        })
        .unwrap();
        draw(cx, window);
        cx.update_window(window, |_, w, _| {
            assert!(w.try_find("files-preview-pane").is_none());
            assert_eq!(w.find("files-list-pane").bounds().size.width, px(420.));
        })
        .unwrap();
    }

    #[::core::prelude::v1::test]
    fn file_search_selects_only_matches_and_shows_an_empty_state() {
        let cx = &mut support::context();
        let (files, _fixture, window, view) = setup(cx);
        draw(cx, window);
        cx.update_window(window, |_, w, cx| {
            view.read(cx).search.read(cx).focus_handle(cx).focus(w, cx);
            w.input("a.txt", cx);
        })
        .unwrap();
        draw(cx, window);
        cx.update_window(window, |_, w, cx| w.click("files-select-all", cx))
            .unwrap();
        view.read_with(cx, |v, _| {
            assert_eq!(
                v.selected,
                BTreeSet::from([files
                    .root
                    .canonicalize()
                    .unwrap()
                    .join("a.txt")
                    .to_string_lossy()
                    .into_owned(),])
            );
            assert!(v.preview.is_none());
        });
        cx.update_window(window, |_, w, cx| {
            w.click("files-clear-selection", cx);
            view.read(cx).search.read(cx).focus_handle(cx).focus(w, cx);
            w.press(
                if cfg!(target_os = "macos") {
                    "cmd-a"
                } else {
                    "ctrl-a"
                },
                cx,
            );
            w.input("missing-file", cx);
        })
        .unwrap();
        draw(cx, window);
        cx.update_window(window, |_, w, _| {
            assert!(w.find("files-empty-state").visible())
        })
        .unwrap();
        view.read_with(cx, |v, _| {
            assert!(v.visible.is_empty());
            assert!(v.selected.is_empty());
        });
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
            w.click("files-close-preview", cx);
        })
        .unwrap();
        view.read_with(cx, |v, cx| {
            assert!(v.preview.is_some());
            assert!(v.dirty(cx));
        });
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
