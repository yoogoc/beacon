//! Read-only Helm release inspector. Storage reads use Beacon's authenticated client.
mod content;
mod view;

use crate::{bridge::Bridge, copyable_text::copyable_text};
use beacon_kube::{
    ClusterSession,
    helm::{self, History, Identity, Revision},
};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _, TitleBar,
    button::{Button, ButtonVariants as _},
    diff::{Diff, DiffMode, DiffState},
    h_flex,
    input::{Editor, EditorState},
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Values,
    Manifest,
    History,
    Compare,
}
impl Page {
    fn label(self) -> &'static str {
        match self {
            Self::Values => "Values",
            Self::Manifest => "Manifest",
            Self::History => "History",
            Self::Compare => "Compare revisions",
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Document {
    Values,
    Manifest,
}
#[derive(Clone)]
struct Choice {
    number: i64,
    label: String,
}
impl SelectItem for Choice {
    type Value = i64;
    fn title(&self) -> SharedString {
        self.label.clone().into()
    }
    fn value(&self) -> &i64 {
        &self.number
    }
    fn matches(&self, query: &str) -> bool {
        self.label.to_lowercase().contains(&query.to_lowercase())
    }
}
type Picker = Entity<SelectState<SearchableVec<Choice>>>;
fn picker(window: &mut Window, cx: &mut App) -> Picker {
    cx.new(|cx| SelectState::new(SearchableVec::new(vec![]), None, window, cx).searchable(true))
}

pub(crate) struct HelmDetails {
    session: Arc<ClusterSession>,
    identity: Identity,
    focus: FocusHandle,
    history: Option<Arc<History>>,
    page: Page,
    document: Document,
    revision: i64,
    from: i64,
    to: i64,
    revision_picker: Picker,
    from_picker: Picker,
    to_picker: Picker,
    editor: Entity<EditorState>,
    diff: Option<Entity<DiffState>>,
    mode: DiffMode,
    wrap: bool,
    reveal: bool,
    loading: bool,
    preparing: bool,
    error: Option<String>,
    content_error: Option<String>,
    generation: u64,
    _load: Option<Task<()>>,
    _prepare: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl Focusable for HelmDetails {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

pub(crate) fn open(
    session: Arc<ClusterSession>,
    identity: Identity,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = Bounds::centered(
        window.display(cx).map(|display| display.id()),
        size(px(1120.), px(760.)),
        cx,
    );
    cx.defer(move |cx| {
        if let Err(error) = open_window(session, identity, bounds, cx) {
            tracing::error!(%error, "could not open Helm release details");
        }
    });
}
fn open_window(
    session: Arc<ClusterSession>,
    identity: Identity,
    bounds: Bounds<Pixels>,
    cx: &mut App,
) -> anyhow::Result<(AnyWindowHandle, Entity<HelmDetails>)> {
    let title = format!("Beacon — Helm · {} / {}", identity.namespace, identity.name);
    gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(700.), px(540.))),
            ..TitleBar::window_options()
        },
        cx,
        move |window, cx| {
            #[cfg(all(test, feature = "ui-tests"))]
            window.set_view_retention(false);
            window.set_window_title(&title);
            window.activate_window();
            cx.new(|cx| HelmDetails::new(session, identity, window, cx))
        },
    )
}
impl HelmDetails {
    fn new(
        session: Arc<ClusterSession>,
        identity: Identity,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let revision_picker = picker(window, cx);
        let from_picker = picker(window, cx);
        let to_picker = picker(window, cx);
        let mut view = Self {
            session,
            identity,
            focus,
            history: None,
            page: Page::Values,
            document: Document::Values,
            revision: 0,
            from: 0,
            to: 0,
            revision_picker,
            from_picker,
            to_picker,
            editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("yaml")
                    .soft_wrap(false)
                    .folding(true)
            }),
            diff: None,
            mode: DiffMode::Split,
            wrap: false,
            reveal: false,
            loading: false,
            preparing: false,
            error: None,
            content_error: None,
            generation: 0,
            _load: None,
            _prepare: None,
            _subscriptions: vec![],
        };
        for (picker, side) in [
            (view.revision_picker.clone(), 0),
            (view.from_picker.clone(), 1),
            (view.to_picker.clone(), 2),
        ] {
            view._subscriptions.push(cx.subscribe_in(
                &picker,
                window,
                move |view, _, event: &SelectEvent<SearchableVec<Choice>>, window, cx| {
                    if let SelectEvent::Confirm(Some(number)) = event {
                        match side {
                            0 => view.revision = *number,
                            1 => view.from = *number,
                            _ => view.to = *number,
                        }
                        view.prepare(window, cx);
                    }
                },
            ));
        }
        view.load(window, cx);
        view
    }
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        self._prepare = None;
        self.preparing = false;
        self.diff = None;
        self.editor
            .update(cx, |editor, cx| editor.set_value("", window, cx));
        self.loading = true;
        self.error = None;
        let session = self.session.clone();
        let identity = self.identity.clone();
        let read =
            Bridge::global(cx).run(async move { helm::history(session.client(), &identity).await });
        self._load = Some(cx.spawn_in(window, async move |view, cx| {
            let result = read.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.loading = false;
                match result {
                    Ok(Ok(history)) => {
                        let first = history.revisions.first().map_or(0, |record| record.number);
                        if !history
                            .revisions
                            .iter()
                            .any(|record| record.number == view.revision)
                        {
                            view.revision = first;
                        }
                        let readable: Vec<_> = history
                            .revisions
                            .iter()
                            .filter(|record| record.revision.is_some())
                            .map(|record| record.number)
                            .collect();
                        if !readable.contains(&view.to) {
                            view.to = readable.first().copied().unwrap_or(0);
                        }
                        if !readable.contains(&view.from) {
                            view.from = readable.get(1).copied().unwrap_or(view.to);
                        }
                        let all: Vec<_> = history
                            .revisions
                            .iter()
                            .map(|record| Choice {
                                number: record.number,
                                label: match &record.revision {
                                    Some(revision) => format!(
                                        "Revision {} · {} · {}",
                                        record.number,
                                        revision.summary.status,
                                        revision.summary.chart
                                    ),
                                    None => format!("Revision {} · Unreadable", record.number),
                                },
                            })
                            .collect();
                        let choices: Vec<_> = all
                            .iter()
                            .filter(|choice| readable.contains(&choice.number))
                            .cloned()
                            .collect();
                        for (picker, choices, number) in [
                            (view.revision_picker.clone(), all, view.revision),
                            (view.from_picker.clone(), choices.clone(), view.from),
                            (view.to_picker.clone(), choices, view.to),
                        ] {
                            picker.update(cx, |picker, cx| {
                                picker.set_items(SearchableVec::new(choices), window, cx);
                                picker.set_selected_value(&number, window, cx);
                            });
                        }
                        view.history = Some(Arc::new(history));
                        view.prepare(window, cx);
                    }
                    Ok(Err(error)) => {
                        view.history = None;
                        view.error = Some(error.user_message());
                    }
                    Err(error) => {
                        view.history = None;
                        view.error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn selected(&self) -> Option<&Revision> {
        let number = if self.page == Page::Compare {
            self.to
        } else {
            self.revision
        };
        self.history
            .as_ref()?
            .revisions
            .iter()
            .find(|record| record.number == number)?
            .revision
            .as_ref()
    }
    fn prepare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self._prepare = None;
        self.diff = None;
        self.content_error = None;
        self.editor
            .update(cx, |editor, cx| editor.set_value("", window, cx));
        let Some(history) = self.history.clone() else {
            return;
        };
        if self.page == Page::History {
            self.preparing = false;
            cx.notify();
            return;
        }
        let page = self.page;
        let document = if page == Page::Manifest {
            Document::Manifest
        } else {
            self.document
        };
        let reveal = self.reveal;
        let revision = self.revision;
        let from = self.from;
        let to = self.to;
        self.preparing = true;
        let task = cx.background_executor().spawn(async move {
            content::prepare(&history, page, document, revision, from, to, reveal)
        });
        self._prepare = Some(cx.spawn_in(window, async move |view, cx| {
            let prepared = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                if view.generation != generation {
                    return;
                }
                view.preparing = false;
                match prepared {
                    Ok(prepared) => {
                        view.editor
                            .update(cx, |editor, cx| editor.set_value(prepared.text, window, cx));
                        if let Some(files) = prepared.files {
                            view.diff = Some(cx.new(|cx| {
                                DiffState::new(files, cx)
                                    .with_mode(view.mode)
                                    .with_context_lines(Some(3))
                            }));
                        }
                    }
                    Err(error) => view.content_error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn select(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        if page == Page::Values {
            self.document = Document::Values;
        }
        self.prepare(window, cx);
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests;
