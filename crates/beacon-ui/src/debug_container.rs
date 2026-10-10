//! A debug-container form and terminal, isolated from the resource overview.
use crate::{bridge::Bridge, copyable_text::copyable_text};
use beacon_kube::{
    ClusterSession, DynamicObject,
    debug::{Options, Profile},
};
use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{SearchableVec, Select, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
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
    message: Option<String>,
    terminal: Option<Entity<crate::terminal::TerminalView>>,
    _task: Option<Task<()>>,
}

pub(crate) fn open(
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DebugView> {
    let context = format!(
        "{} · {}/{}",
        session.id().display_name(),
        object.metadata.namespace.as_deref().unwrap_or(""),
        object.metadata.name.as_deref().unwrap_or("")
    );
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
            message: None,
            terminal: None,
            _task: None,
        }
    });
    let content = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(format!("Debug Pod · {context}"))
            .width(px(1050.))
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
        if self.busy {
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
                beacon_kube::debug::create(session, object, request)
                    .await
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
                    view.message = Some(error);
                    cx.notify();
                    None
                }
                Err(error) => {
                    view.busy = false;
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
            let ready = match cx.update(|_, cx| {
                let session = session.clone();
                let container = options.name.clone();
                Bridge::global(cx).run_cancellable(async move {
                    beacon_kube::debug::wait_running(session, object, container)
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
                            terminal.set_command(&options.shell, window, cx);
                            terminal.start(window, cx);
                        });
                        view.terminal = Some(terminal);
                        view.message = None;
                    }
                    Ok(Err(error)) => view.message = Some(error),
                    Err(error) => view.message = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for DebugView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locked = self.busy || self.created.is_some();
        v_flex().w_full().h((window.viewport_size().height * 0.7).min(px(620.))).gap_3()
            .child(h_flex().gap_3()
                .child(v_flex().flex_1().gap_1().child("Container name").child(Input::new(&self.name).small().disabled(locked)))
                .child(v_flex().flex_1().gap_1().child("Image").child(Input::new(&self.image).small().disabled(locked))))
            .child(h_flex().gap_3()
                .child(v_flex().flex_1().gap_1().child("Target container").child(Select::new(&self.target).small().disabled(locked)))
                .child(v_flex().flex_1().gap_1().child("Shell executable").child(Input::new(&self.shell).small().disabled(locked))))
            .child(h_flex().gap_2().child("Security profile")
                .child(Button::new("debug-profile-restricted").small().ghost().label("Restricted").selected(self.profile == Profile::Restricted).disabled(locked).on_click(cx.listener(|view, _, _, cx| { view.profile = Profile::Restricted; cx.notify(); })))
                .child(Button::new("debug-profile-baseline").small().ghost().label("Baseline").selected(self.profile == Profile::Baseline).disabled(locked).on_click(cx.listener(|view, _, _, cx| { view.profile = Profile::Baseline; cx.notify(); }))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Adds an ephemeral container to this Pod. Its entry remains until the Pod is removed. Restricted runs as user 1000 with all capabilities dropped."))
            .child(Button::new("start-debug-container").small().primary().label(if self.created.is_some() { "Open debug terminal" } else { "Create and open terminal" }).disabled(self.busy).on_click(cx.listener(|view, _, window, cx| view.start(window, cx))))
            .children(self.message.as_ref().map(|message| copyable_text("debug-message", message.clone())))
            .when_some(self.terminal.clone(), |view, terminal| view.child(div().flex_1().min_h_0().child(terminal)))
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
    }
}
