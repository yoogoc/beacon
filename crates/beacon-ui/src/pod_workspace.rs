//! A bounded log buffer and tool state per Pod, retained until its tab is closed.
use crate::pod_tools::{PodToolTab, PodToolsClosed, PodToolsView};
use beacon_kube::{ClusterSession, DynamicObject, ObjectRef, Rules};
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::Cell, rc::Rc, sync::Arc};

struct Session {
    id: u64,
    target: ObjectRef,
    uid: Option<String>,
    tools: Entity<PodToolsView>,
    _close: Subscription,
}

pub(crate) struct PodWorkspace {
    session: Arc<ClusterSession>,
    sessions: Vec<Session>,
    next: u64,
    primary: Option<u64>,
    secondary: Option<u64>,
    choices: Vec<Arc<DynamicObject>>,
    picker: Entity<InputState>,
    picker_open: bool,
    width: Rc<Cell<Pixels>>,
    split: Entity<ResizableState>,
    _picker: Subscription,
    closing: Option<u64>,
    rules: Option<Arc<Rules>>,
}
pub(crate) struct Closed;
impl EventEmitter<Closed> for PodWorkspace {}

fn choice_label(object: &DynamicObject) -> String {
    ObjectRef::of(object).to_string()
}

fn tab_label(name: &str) -> String {
    let chars = name.chars().collect::<Vec<_>>();
    if chars.len() <= 36 {
        name.to_owned()
    } else {
        format!(
            "{}…{}",
            chars[..22].iter().collect::<String>(),
            chars[chars.len() - 10..].iter().collect::<String>()
        )
    }
}

impl PodWorkspace {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker = cx.new(|cx| crate::i18n::input("Search Pods", window, cx).clean_on_escape());
        let subscription =
            cx.subscribe_in(&picker, window, |view, _, event, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    let query = view.picker.read(cx).value().to_lowercase();
                    let first = view
                        .choices
                        .iter()
                        .find(|object| choice_label(object).to_lowercase().contains(query.trim()))
                        .cloned();
                    if let Some(object) = first {
                        view.picker_open = false;
                        view.open(object, view.rules.clone(), PodToolTab::Logs, window, cx);
                    }
                }
                _ => {}
            });
        Self {
            session,
            sessions: vec![],
            next: 0,
            primary: None,
            secondary: None,
            choices: vec![],
            picker,
            picker_open: false,
            width: Rc::new(Cell::new(px(800.))),
            split: cx.new(|_| ResizableState::default()),
            _picker: subscription,
            closing: None,
            rules: None,
        }
    }

    pub(crate) fn has_active_session(&self, cx: &App) -> bool {
        self.sessions
            .iter()
            .any(|session| session.tools.read(cx).has_active_session(cx))
    }
    pub(crate) fn shell_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.sessions
            .iter()
            .any(|session| session.tools.read(cx).shell_has_focus(window, cx))
    }
    pub(crate) fn refresh(
        &mut self,
        objects: Vec<Arc<DynamicObject>>,
        rules: Option<Arc<Rules>>,
        cx: &mut Context<Self>,
    ) {
        self.rules = rules.clone();
        for entry in &self.sessions {
            if let Some(object) = objects.iter().find(|object| {
                ObjectRef::of(object) == entry.target && object.metadata.uid == entry.uid
            }) {
                entry.tools.update(cx, |tools, cx| {
                    tools.refresh(object.clone(), rules.clone(), cx)
                });
            }
        }
        self.choices = objects;
        cx.notify();
    }

    pub(crate) fn open(
        &mut self,
        object: Arc<DynamicObject>,
        rules: Option<Arc<Rules>>,
        tab: PodToolTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = ObjectRef::of(&object);
        if let Some(entry) = self
            .sessions
            .iter()
            .find(|entry| entry.target == target && entry.uid == object.metadata.uid)
        {
            let id = entry.id;
            entry.tools.update(cx, |tools, cx| {
                tools.refresh(object, rules, cx);
                tools.select(tab, window, cx);
            });
            self.activate(id, window, cx);
            return;
        }
        let id = self.next;
        self.next += 1;
        let uid = object.metadata.uid.clone();
        let session = self.session.clone();
        let tools = cx.new(|cx| {
            let mut tools = PodToolsView::new(session, object, rules, tab, window, cx);
            tools.embed(self.secondary.is_some(), cx);
            tools
        });
        let close = cx.subscribe_in(
            &tools,
            window,
            move |view, _, _: &PodToolsClosed, window, cx| view.request_close(id, window, cx),
        );
        self.sessions.push(Session {
            id,
            target,
            uid,
            tools,
            _close: close,
        });
        self.activate(id, window, cx);
    }

    fn activate(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sessions.iter().any(|entry| entry.id == id) {
            return;
        }
        if self.secondary == Some(id) {
            self.secondary = self.primary;
        }
        self.primary = Some(id);
        self.sync_headers(cx);
        if let Some(entry) = self.sessions.iter().find(|entry| entry.id == id) {
            entry
                .tools
                .update(cx, |tools, cx| tools.focus_current(window, cx));
        }
        cx.notify();
    }
    fn sync_headers(&self, cx: &mut App) {
        for entry in &self.sessions {
            entry
                .tools
                .update(cx, |tools, cx| tools.embed(self.secondary.is_some(), cx));
        }
    }
    fn toggle_split(&mut self, cx: &mut Context<Self>) {
        self.secondary = if self.secondary.is_some() {
            None
        } else {
            self.sessions
                .iter()
                .find(|entry| Some(entry.id) != self.primary)
                .map(|entry| entry.id)
        };
        self.sync_headers(cx);
        cx.notify();
    }
    pub(crate) fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.primary {
            self.request_close(id, window, cx);
        }
    }
    fn request_close(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.sessions.iter().find(|entry| entry.id == id) else {
            return;
        };
        if entry.tools.read(cx).has_active_session(cx) {
            if self.closing.is_some() {
                return;
            }
            self.closing = Some(id);
            let answer = window.prompt(
                PromptLevel::Warning,
                crate::i18n::text("Close Pod session?").as_ref(),
                Some(
                    crate::i18n::text(
                        "The running command or shell in this session will disconnect.",
                    )
                    .as_ref(),
                ),
                &[
                    PromptButton::cancel(crate::i18n::text("Cancel").as_ref()),
                    PromptButton::ok(crate::i18n::text("Close").as_ref()),
                ],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                let result = answer.await;
                let _ = this.update_in(cx, |view, window, cx| {
                    view.closing = None;
                    if matches!(result, Ok(1)) {
                        view.close(id, window, cx);
                    }
                });
            })
            .detach();
        } else {
            self.close(id, window, cx);
        }
    }
    fn close(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.sessions.iter().position(|entry| entry.id == id) else {
            return;
        };
        let entry = self.sessions.remove(index);
        entry.tools.update(cx, |tools, cx| tools.stop(cx));
        if self.primary == Some(id) {
            self.primary = self
                .sessions
                .get(index.min(self.sessions.len().saturating_sub(1)))
                .map(|entry| entry.id);
        }
        if self.secondary == Some(id) || self.secondary == self.primary {
            self.secondary = self
                .sessions
                .iter()
                .find(|entry| Some(entry.id) != self.primary)
                .map(|entry| entry.id);
        }
        if self.sessions.len() < 2 {
            self.secondary = None;
        }
        self.sync_headers(cx);
        if self.sessions.is_empty() {
            cx.emit(Closed);
        } else if let Some(active) = self.primary {
            self.activate(active, window, cx);
        }
        cx.notify();
    }

    fn pod_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let input = self.picker.clone();
        let query = input.read(cx).value().to_lowercase();
        let choices = self
            .choices
            .iter()
            .filter(|object| choice_label(object).to_lowercase().contains(query.trim()))
            .cloned()
            .collect::<Vec<_>>();
        let owner = cx.entity().downgrade();
        let opening = owner.clone();
        Popover::new("add-pod-session")
            .p_1p5()
            .open(self.picker_open)
            .track_focus(&input.read(cx).focus_handle(cx))
            .on_open_change(move |open, window, cx| {
                let _ = opening.update(cx, |view, cx| {
                    view.picker_open = *open;
                    if *open {
                        view.picker
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    cx.notify();
                });
            })
            .trigger(
                Button::new("add-pod-session-button")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Plus)
                    .accessibility_label(crate::i18n::text("Open another Pod session"))
                    .tooltip(crate::i18n::text("Open another Pod session")),
            )
            .content(move |_, _, cx| {
                let count = choices.len();
                let choices = choices.clone();
                let owner = owner.clone();
                v_flex()
                    .w(px(360.))
                    .max_w_full()
                    .gap_1()
                    .child(Input::new(&input).id("pod-session-picker").small())
                    .child(
                        div().h(px((count as f32 * 34.).clamp(34., 272.))).child(
                            uniform_list("pod-session-choices", count, move |range, _, _cx| {
                                range
                                    .map(|index| {
                                        let object = choices[index].clone();
                                        let label = choice_label(&object);
                                        let owner = owner.clone();
                                        Button::new(("open-pod-session", index))
                                            .ghost()
                                            .small()
                                            .w_full()
                                            .h(px(34.))
                                            .justify_start()
                                            .child(div().min_w_0().truncate().child(label.clone()))
                                            .accessibility_label(label.clone())
                                            .tooltip(label)
                                            .on_click(move |_, window, cx| {
                                                let _ = owner.update(cx, |view, cx| {
                                                    view.picker_open = false;
                                                    view.open(
                                                        object.clone(),
                                                        view.rules.clone(),
                                                        PodToolTab::Logs,
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            })
                                            .into_any_element()
                                    })
                                    .collect()
                            })
                            .size_full(),
                        ),
                    )
                    .when(count == 0, |panel| {
                        panel.child(
                            div()
                                .p_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::text("No matching Pods")),
                        )
                    })
            })
    }

    fn session_menu(&self, secondary: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let entries = self
            .sessions
            .iter()
            .map(|entry| (entry.id, entry.target.to_string()))
            .collect::<Vec<_>>();
        let active = self.primary;
        let selected = if secondary {
            self.secondary
        } else {
            self.primary
        };
        Button::new(if secondary {
            "pod-session-right"
        } else {
            "pod-session-list"
        })
        .xsmall()
        .ghost()
        .icon(if secondary {
            IconName::ArrowLeftRight
        } else {
            IconName::List
        })
        .tooltip(crate::i18n::text(if secondary {
            "Choose right-hand Pod"
        } else {
            "All Pod sessions"
        }))
        .dropdown_menu(move |mut menu, _, _| {
            for (id, label) in &entries {
                if secondary && Some(*id) == active {
                    continue;
                }
                let weak = weak.clone();
                let id = *id;
                menu = menu.item(
                    PopupMenuItem::new(label.clone())
                        .checked(selected == Some(id))
                        .on_click(move |_, window, cx| {
                            let _ = weak.update(cx, |view, cx| {
                                if secondary {
                                    view.secondary = Some(id);
                                    view.sync_headers(cx);
                                    cx.notify();
                                } else {
                                    view.activate(id, window, cx);
                                }
                            });
                        }),
                );
            }
            menu
        })
    }
}

impl Render for PodWorkspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let limit = if self.width.get() < px(620.) { 2 } else { 4 };
        let mut shown = self
            .sessions
            .iter()
            .take(limit)
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        if let Some(active) = self.primary
            && !shown.contains(&active)
        {
            shown.pop();
            shown.push(active);
        }
        let tabs = shown
            .into_iter()
            .filter_map(|id| self.sessions.iter().find(|entry| entry.id == id))
            .map(|entry| {
                let id = entry.id;
                let selected = self.primary == Some(id);
                h_flex()
                    .id(("pod-session", id as usize))
                    .test_support()
                    .aria_label(entry.target.to_string())
                    .flex_1()
                    .min_w_0()
                    .max_w(px(250.))
                    .items_center()
                    .border_b_2()
                    .border_color(if selected {
                        cx.theme().primary
                    } else if self.secondary == Some(id) {
                        cx.theme().muted_foreground
                    } else {
                        cx.theme().transparent
                    })
                    .when(selected, |this| this.bg(cx.theme().muted.opacity(0.4)))
                    .child(
                        Button::new(("activate-pod-session", id as usize))
                            .ghost()
                            .small()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .accessibility_label(entry.target.to_string())
                            .tooltip(entry.target.to_string())
                            .child(div().w_full().truncate().text_xs().child(
                                if self.sessions.iter().any(|other| {
                                    other.target.name == entry.target.name
                                        && other.target.namespace != entry.target.namespace
                                }) {
                                    format!(
                                        "{}/{}",
                                        entry.target.namespace.as_deref().unwrap_or_default(),
                                        tab_label(&entry.target.name)
                                    )
                                } else {
                                    tab_label(&entry.target.name)
                                },
                            ))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.activate(id, window, cx)
                            })),
                    )
                    .child(
                        Button::new(("close-pod-session", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip(crate::i18n::text("Close this Pod session"))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.request_close(id, window, cx)
                            })),
                    )
            })
            .collect::<Vec<_>>();
        let toolbar = h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(tabs)
            .child(self.pod_picker(cx))
            .child(self.session_menu(false, cx))
            .when(self.secondary.is_some(), |bar| {
                bar.child(self.session_menu(true, cx))
            })
            .child(
                Button::new("split-pod-sessions")
                    .xsmall()
                    .ghost()
                    .selected(self.secondary.is_some())
                    .toggled(self.secondary.is_some())
                    .icon(IconName::Columns2)
                    .disabled(self.sessions.len() < 2)
                    .tooltip(crate::i18n::text(if self.secondary.is_some() {
                        "Return to a single Pod"
                    } else {
                        "View two Pods side by side"
                    }))
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_split(cx))),
            );
        let primary = self
            .sessions
            .iter()
            .find(|entry| Some(entry.id) == self.primary)
            .map(|entry| entry.tools.clone());
        let secondary = self
            .sessions
            .iter()
            .find(|entry| Some(entry.id) == self.secondary)
            .map(|entry| entry.tools.clone());
        let content = match (primary, secondary) {
            (Some(left), Some(right)) => {
                let right = div()
                    .id("pod-session-secondary")
                    .test_support()
                    .size_full()
                    .min_size_0()
                    .child(right);
                let left = div()
                    .id("pod-session-primary")
                    .test_support()
                    .size_full()
                    .min_size_0()
                    .child(left);
                if self.width.get() < px(620.) {
                    v_resizable("pod-session-panes-vertical")
                        .with_state(&self.split)
                        .child(resizable_panel().child(left))
                        .child(resizable_panel().child(right))
                        .into_any_element()
                } else {
                    h_resizable("pod-session-panes")
                        .with_state(&self.split)
                        .child(resizable_panel().child(left))
                        .child(resizable_panel().child(right))
                        .into_any_element()
                }
            }
            (Some(view), _) => div()
                .id("pod-session-primary")
                .test_support()
                .size_full()
                .min_size_0()
                .child(view)
                .into_any_element(),
            _ => div().into_any_element(),
        };
        let width = self.width.clone();
        let weak = cx.entity().downgrade();
        v_flex()
            .id("pod-session-workspace")
            .test_support()
            .relative()
            .size_full()
            .min_size_0()
            .bg(cx.theme().background)
            .child(toolbar)
            .child(div().flex_1().min_size_0().overflow_hidden().child(content))
            .child(
                canvas(
                    move |bounds, _, _| {
                        let old = width.replace(bounds.size.width);
                        (old < px(620.)) != (bounds.size.width < px(620.))
                    },
                    move |_, changed, _, cx| {
                        if changed {
                            let _ = weak.update(cx, |_, cx| cx.notify());
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests;
