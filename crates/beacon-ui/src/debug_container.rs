//! A debug-container form and terminal, isolated from the resource overview.
use crate::{bridge::Bridge, copyable_text::copyable_text};
use beacon_kube::{
    ClusterSession, DynamicObject,
    debug::{Options, Profile},
};
use futures::StreamExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{SearchableVec, Select, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::component::{Icon, spinner::Spinner};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

pub(crate) struct DebugView {
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    name: Entity<InputState>,
    image: Entity<InputState>,
    shell: Entity<InputState>,
    target: Entity<SelectState<SearchableVec<String>>>,
    profile: Profile,
    created: Option<Options>,
    busy: bool,
    failed: bool,
    advanced: bool,
    message: Option<String>,
    terminal: Option<Entity<crate::terminal::TerminalView>>,
    stopped: bool,
    _stop_dialog: Option<Entity<crate::debug_stop::StopDebug>>,
    _stop_subscription: Option<Subscription>,
    _task: Option<Task<()>>,
    _progress: Option<Task<()>>,
}

pub(crate) fn open(
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DebugView> {
    let view = cx.new(|cx| {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let choices: Vec<String> = object
            .data
            .pointer("/spec/containers")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|container| container["name"].as_str().map(str::to_owned))
            .collect();
        DebugView {
            session,
            object,
            name: cx.new(|cx| InputState::new(window, cx).default_value(format!("debug-{suffix}"))),
            image: cx.new(|cx| InputState::new(window, cx).default_value("busybox:stable")),
            shell: cx.new(|cx| InputState::new(window, cx).default_value("/bin/sh")),
            target: cx.new(|cx| {
                let selected = choices.first().cloned();
                let mut state = SelectState::new(SearchableVec::new(choices), None, window, cx)
                    .searchable(true);
                if let Some(selected) = selected {
                    state.set_selected_value(&selected, window, cx);
                }
                state
            }),
            profile: Profile::Restricted,
            created: None,
            busy: false,
            failed: false,
            advanced: false,
            message: None,
            terminal: None,
            stopped: false,
            _stop_dialog: None,
            _stop_subscription: None,
            _task: None,
            _progress: None,
        }
    });
    let content = view.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        dialog
            .title(crate::i18n::text("Debug Pod"))
            .on_ok(|_, _, _| false)
            .width(if content.read(cx).terminal.is_some() {
                px(980.)
            } else {
                px(640.)
            })
            .child(content.clone())
    });
    view
}

impl DebugView {
    pub(crate) fn busy(&self) -> bool {
        self.busy
    }
    pub(crate) fn has_active_terminal(&self, cx: &App) -> bool {
        self.terminal
            .as_ref()
            .is_some_and(|terminal| terminal.read(cx).is_active())
    }
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.stopped {
            return;
        }
        let options = self.created.clone().unwrap_or_else(|| Options {
            name: self.name.read(cx).value().trim().into(),
            image: self.image.read(cx).value().trim().into(),
            shell: self.shell.read(cx).value().trim().into(),
            target: self.target.read(cx).selected_value().cloned(),
            profile: self.profile,
        });
        self.busy = true;
        self.failed = false;
        self.message = Some(
            if self.created.is_some() {
                "Waiting for the debug container…"
            } else {
                "Creating debug container…"
            }
            .into(),
        );
        let session = self.session.clone();
        let object = self.object.clone();
        let request = options.clone();
        let create = self.created.is_none();
        let creating = Bridge::global(cx).run(async move {
            if create {
                tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    beacon_kube::debug::create(session, object, request),
                )
                .await
                .map_err(|_| {
                    "Timed out creating the debug container. Check the Pod before retrying."
                        .to_owned()
                })?
                .map_err(|error| error.user_message())
            } else {
                Ok(())
            }
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let created = creating.await;
            let session = match this.update(cx, |view, cx| match created {
                Ok(Ok(())) => {
                    view.created = Some(options.clone());
                    view.message = Some("Container created. Waiting for it to start…".into());
                    cx.notify();
                    Some(view.session.clone())
                }
                Ok(Err(error)) => {
                    view.busy = false;
                    view.failed = true;
                    view.message = Some(error);
                    cx.notify();
                    None
                }
                Err(error) => {
                    view.busy = false;
                    view.failed = true;
                    view.message = Some(error.to_string());
                    cx.notify();
                    None
                }
            }) {
                Ok(Some(session)) => session,
                _ => return,
            };
            let object = match this.read_with(cx, |view, _| view.object.clone()) {
                Ok(object) => object,
                Err(_) => return,
            };
            let (progress, mut updates) = futures::channel::mpsc::unbounded::<String>();
            let _ = this.update_in(cx, |view, window, cx| {
                view._progress = Some(cx.spawn_in(window, async move |this, cx| {
                    while let Some(message) = updates.next().await {
                        let _ = this.update(cx, |view, cx| {
                            view.message = Some(message);
                            cx.notify();
                        });
                    }
                }));
            });
            let ready = match cx.update(|_, cx| {
                let session = session.clone();
                let container = options.name.clone();
                Bridge::global(cx).run_cancellable(async move {
                    beacon_kube::debug::wait_running_with_progress(
                        session,
                        object,
                        container,
                        move |reason, message| {
                            let _ = progress.unbounded_send(if message.is_empty() {
                                reason.to_owned()
                            } else {
                                format!("{reason}: {message}")
                            });
                        },
                    )
                    .await
                    .map_err(|error| error.user_message())
                })
            }) {
                Ok(ready) => ready,
                Err(_) => return,
            };
            let result = ready.result().await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.busy = false;
                view._progress = None;
                match result {
                    Ok(Ok(())) => {
                        let namespace = view.object.metadata.namespace.clone().unwrap_or_default();
                        let pod = view.object.metadata.name.clone().unwrap_or_default();
                        let terminal = cx.new(|cx| {
                            crate::terminal::TerminalView::new(
                                session,
                                namespace,
                                pod,
                                Some(options.name.clone()),
                                window,
                                cx,
                            )
                        });
                        terminal.update(cx, |terminal, cx| {
                            terminal.set_shell(&options.shell, window, cx);
                            terminal.start(window, cx);
                        });
                        view.terminal = Some(terminal);
                        view.message = None;
                    }
                    Ok(Err(error)) => {
                        view.failed = true;
                        view.message = Some(error);
                    }
                    Err(error) => {
                        view.failed = true;
                        view.message = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn stop_waiting(&mut self, cx: &mut Context<Self>) {
        self._task = None;
        self._progress = None;
        self.busy = false;
        self.failed = false;
        self.message = Some(
            "Waiting stopped. The container remains in the Pod; retry to open its terminal.".into(),
        );
        cx.notify();
    }

    fn request_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(created) = &self.created else {
            return;
        };
        if self.busy || self.stopped {
            return;
        }
        let dialog = crate::debug_stop::open(
            self.session.clone(),
            self.object.clone(),
            created.name.clone(),
            window,
            cx,
        );
        self._stop_subscription = Some(cx.subscribe(
            &dialog,
            |view, _, _: &crate::debug_stop::Stopped, cx| {
                view.stopped = true;
                view.failed = false;
                view.message = Some("Debug container stopped".into());
                if let Some(terminal) = &view.terminal {
                    terminal.update(cx, |terminal, cx| terminal.debug_container_stopped(cx));
                }
                cx.notify();
            },
        ));
        self._stop_dialog = Some(dialog);
    }
}

impl Render for DebugView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locked = self.busy || self.created.is_some();
        let header = h_flex()
            .gap_3()
            .p_3()
            .rounded_lg()
            .bg(cx.theme().muted.opacity(0.45))
            .child(
                div()
                    .size(px(34.))
                    .rounded_md()
                    .bg(cx.theme().background)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(IconName::Terminal)
                            .size(px(18.))
                            .text_color(cx.theme().primary),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_ellipsis()
                            .child(self.object.metadata.name.clone().unwrap_or_default()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}",
                                self.session.id().display_name(),
                                self.object.metadata.namespace.as_deref().unwrap_or("")
                            )),
                    ),
            );
        let mut view = v_flex()
            .id("debug-container")
            .w_full()
            .min_h_0()
            .max_h((window.viewport_size().height - px(180.)).max(px(240.)))
            .overflow_y_scroll()
            .gap_4()
            .child(header);
        if let Some(created) = &self.created {
            view =
                view.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .min_w_0()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(created.name.clone()),
                        )
                        .when(!self.stopped, |view| {
                            view.child(
                                Button::new("stop-debug-container")
                                    .small()
                                    .ghost()
                                    .icon(IconName::CircleX)
                                    .label(crate::i18n::text("Stop debug container"))
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.request_stop(window, cx)
                                    })),
                            )
                        }),
                );
        }
        if let Some(terminal) = &self.terminal {
            return view.child(div().h(px(400.)).min_h_0().child(terminal.clone()));
        }
        let advanced = v_flex()
            .p_3()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(crate::i18n::text("Container name")),
                            )
                            .child(Input::new(&self.name).small().disabled(locked)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(crate::i18n::text("Shell executable")),
                            )
                            .child(Input::new(&self.shell).small().disabled(locked)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::i18n::text("Security profile")),
                    )
                    .child(
                        h_flex()
                            .p_0p5()
                            .rounded_md()
                            .bg(cx.theme().muted)
                            .child(
                                Button::new("debug-profile-restricted")
                                    .xsmall()
                                    .ghost()
                                    .label(crate::i18n::text("Restricted"))
                                    .selected(self.profile == Profile::Restricted)
                                    .disabled(locked)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.profile = Profile::Restricted;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("debug-profile-baseline")
                                    .xsmall()
                                    .ghost()
                                    .label(crate::i18n::text("Baseline"))
                                    .selected(self.profile == Profile::Baseline)
                                    .disabled(locked)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.profile = Profile::Baseline;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        view = view.child(
            h_flex()
                .gap_3()
                .items_start()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1p5()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::text("Image")),
                        )
                        .child(Input::new(&self.image).small().disabled(locked)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1p5()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::text("Target container")),
                        )
                        .child(Select::new(&self.target).small().disabled(locked)),
                ),
        );
        view = view.child(h_flex().gap_2()
            .child(Button::new("debug-advanced").xsmall().ghost().icon(if self.advanced { IconName::ChevronDown } else { IconName::ChevronRight })
                .label(crate::i18n::text("Advanced options")).on_click(cx.listener(|view,_,_,cx|{view.advanced = !view.advanced;cx.notify();})))
            .child(div().flex_1())
            .child(Button::new("debug-container-info").xsmall().ghost().icon(IconName::Info)
                .tooltip(crate::i18n::text("Adds an ephemeral container to this Pod. Its entry remains until the Pod is removed. Restricted runs as user 1000 with all capabilities dropped."))))
            .when(self.advanced,|view|view.child(advanced));
        if let Some(message) = &self.message {
            view = view.child(
                h_flex()
                    .id("debug-startup-status")
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .min_w_0()
                    .bg(if self.failed {
                        cx.theme().danger.opacity(0.06)
                    } else {
                        cx.theme().muted.opacity(0.5)
                    })
                    .when(self.busy, |row| row.child(Spinner::new().small()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(if self.failed {
                                cx.theme().danger
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(copyable_text("debug-message", crate::i18n::text(message))),
                    ),
            );
        }
        view.child(
            h_flex()
                .pt_3()
                .gap_2()
                .justify_end()
                .border_t_1()
                .border_color(cx.theme().border)
                .when(self.busy && self.created.is_some(), |row| {
                    row.child(
                        Button::new("debug-stop-waiting")
                            .small()
                            .ghost()
                            .label(crate::i18n::text("Stop waiting"))
                            .on_click(cx.listener(|view, _, _, cx| view.stop_waiting(cx))),
                    )
                })
                .child(
                    Button::new("start-debug-container")
                        .small()
                        .primary()
                        .icon(IconName::Terminal)
                        .label(crate::i18n::text(if self.stopped {
                            "Debug container stopped"
                        } else if self.busy {
                            "Waiting for the debug container…"
                        } else if self.created.is_some() {
                            "Open debug terminal"
                        } else {
                            "Create and open terminal"
                        }))
                        .disabled(self.busy || self.stopped)
                        .on_click(cx.listener(|view, _, window, cx| view.start(window, cx))),
                ),
        )
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    #[::core::prelude::v1::test]
    fn the_form_creates_one_container_and_opens_its_terminal() {
        let cx = &mut support::context();
        let object = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web","namespace":"default","uid":"one","resourceVersion":"42"},"spec":{"containers":[{"name":"app","image":"distroless"}]},"status":{"phase":"Running"}});
        let (fixture, session) = support::fixture(cx, "debug-form-fixture", vec![object.clone()]);
        let window = support::window(cx);
        let view = cx
            .update_window(window, |_, window, cx| {
                open(
                    session,
                    Arc::new(serde_json::from_value(object).unwrap()),
                    window,
                    cx,
                )
            })
            .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("start-debug-container", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| view.terminal.is_some() && !view.busy)
        });
        let actual = fixture.object("Pod", "web");
        assert_eq!(
            actual["spec"]["ephemeralContainers"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            actual["spec"]["ephemeralContainers"][0]["targetContainerName"],
            "app"
        );
        assert_eq!(
            actual["spec"]["ephemeralContainers"][0]["securityContext"]["runAsNonRoot"],
            true
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        assert_eq!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(request, _)| request.starts_with("PATCH"))
                .count(),
            1
        );
        let terminal = view.read_with(cx, |view, _| view.terminal.clone().unwrap());
        cx.update_window(window, |_, window, cx| {
            terminal.update(cx, |terminal, cx| terminal.feed(b"saved debug output", cx));
            window.render_frame(cx);
            window.click("stop-debug-container", cx);
            window.render_frame(cx);
            window.click("debug-stop-cancel", cx);
        })
        .unwrap();
        assert!(!view.read_with(cx, |view, _| view.stopped));
        let mut terminated = fixture.object("Pod", "web");
        terminated["status"]["ephemeralContainerStatuses"][0]["state"] =
            json!({"terminated":{"exitCode":0}});
        fixture.put(terminated);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("stop-debug-container", cx);
            window.render_frame(cx);
            window.click("debug-stop-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.stopped));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("debug-stop-cancel", cx);
            window.render_frame(cx);
            assert!(window.try_find("stop-debug-container").is_none());
            assert!(window.try_find("start-shell").is_none());
            assert!(window.find("terminal-grid").visible());
            assert!(!view.read(cx).has_active_terminal(cx));
            window.right_click("terminal-grid", cx);
        })
        .unwrap();
        support::render_until(cx, window, |window| {
            gpui_kit::base::test_support::snapshots(window)
                .iter()
                .any(|item| {
                    item.role() == Some(Role::MenuItem)
                        && item.label() == Some("Copy terminal output")
                })
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let copy = gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .find(|item| {
                    item.role() == Some(Role::MenuItem)
                        && item.label() == Some("Copy terminal output")
                })
                .unwrap();
            window.click(copy.path().last().unwrap().clone(), cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(cx.read(|cx| {
            cx.read_from_clipboard()
                .unwrap()
                .text()
                .unwrap()
                .contains("saved debug output")
        }));
    }
}
