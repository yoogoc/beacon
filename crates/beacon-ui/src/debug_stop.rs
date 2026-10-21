//! Confirm and stop a managed debug container without closing its transcript.
use crate::{bridge::Bridge, copyable_text::copyable_text};
use beacon_kube::{ClusterSession, DynamicObject};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex,
    spinner::Spinner, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

pub(crate) struct Stopped;
impl EventEmitter<Stopped> for StopDebug {}

pub(crate) struct StopDebug {
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    container: String,
    busy: bool,
    done: bool,
    error: Option<String>,
    _task: Option<Task<()>>,
}

pub(crate) fn open(
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    container: String,
    window: &mut Window,
    cx: &mut App,
) -> Entity<StopDebug> {
    let view = cx.new(|_| StopDebug {
        session,
        object,
        container,
        busy: false,
        done: false,
        error: None,
        _task: None,
    });
    let content = view.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        dialog
            .title(crate::i18n::text("Stop debug container?"))
            .width(px(460.))
            .close_button(!content.read(cx).busy)
            .on_ok(|_, _, _| false)
            .on_cancel({
                let content = content.clone();
                move |_, _, cx| !content.read(cx).busy
            })
            .child(content.clone())
    });
    view
}

impl StopDebug {
    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.done {
            return;
        }
        self.busy = true;
        self.error = None;
        let session = self.session.clone();
        let object = self.object.clone();
        let container = self.container.clone();
        let stopping = Bridge::global(cx).run(async move {
            beacon_kube::debug::stop(session, object, container)
                .await
                .map_err(|error| error.user_message())
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = stopping.await;
            let _ = this.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(Ok(_)) => {
                        view.done = true;
                        cx.emit(Stopped);
                    }
                    Ok(Err(error)) => view.error = Some(error),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for StopDebug {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().id("debug-stop-confirmation").gap_3().min_w_0()
            .child(v_flex().p_3().gap_1().rounded_md().bg(cx.theme().muted.opacity(0.5))
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(self.container.clone()))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} · {} / {}", self.session.id().display_name(), self.object.metadata.namespace.as_deref().unwrap_or_default(), self.object.metadata.name.as_deref().unwrap_or_default()))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text(if self.done {
                "The debug container has stopped. Its record remains until the Pod is replaced."
            } else { "Stops this debug container and its sessions. Application containers keep running. The debug container cannot be restarted." })))
            .when(self.done, |view| view.child(h_flex().gap_2().items_center()
                .child(Icon::new(IconName::CircleCheck).size_4().text_color(cx.theme().success))
                .child(div().text_sm().child(crate::i18n::text("Debug container stopped")))))
            .when_some(self.error.clone(), |view, error| view.child(div().text_xs().text_color(cx.theme().danger).child(copyable_text("debug-stop-error", crate::i18n::text(error)))))
            .child(h_flex().gap_2().items_center().justify_end()
                .when(self.busy, |view| view.child(Spinner::new().small()))
                .child(Button::new("debug-stop-cancel").small().ghost()
                    .disabled(self.busy).label(crate::i18n::text(if self.done { "Done" } else { "Cancel" }))
                    .on_click(|_, window, cx| window.close_dialog(cx)))
                .when(!self.done, |view| view.child(Button::new("debug-stop-confirm").small().danger().disabled(self.busy)
                    .label(crate::i18n::text(if self.busy { "Stopping…" } else { "Stop debug container" }))
                    .on_click(cx.listener(|view, _, window, cx| view.stop(window, cx))))))
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;

    #[::core::prelude::v1::test]
    fn cancel_never_stops_a_container_and_success_requires_a_verified_terminated_state() {
        let cx = &mut support::context();
        let object = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web","namespace":"default","uid":"one"},"spec":{"containers":[{"name":"app","image":"fixture"}],"ephemeralContainers":[{"name":"debugger","image":"busybox"}]},"status":{"phase":"Running","ephemeralContainerStatuses":[{"name":"debugger","image":"busybox","imageID":"fixture","ready":false,"restartCount":0,"state":{"terminated":{"exitCode":0}}}]}});
        let (fixture, session) = support::fixture(cx, "debug-stop-dialog", vec![object.clone()]);
        let window = support::window(cx);
        let object: Arc<DynamicObject> = Arc::new(serde_json::from_value(object).unwrap());
        cx.update_window(window, |_, window, cx| {
            open(
                session.clone(),
                object.clone(),
                "debugger".into(),
                window,
                cx,
            );
            window.render_frame(cx);
            window.click("debug-stop-cancel", cx);
        })
        .unwrap();
        assert!(fixture.requests.lock().unwrap().is_empty());
        let view = cx
            .update_window(window, |_, window, cx| {
                let view = open(session, object, "debugger".into(), window, cx);
                window.render_frame(cx);
                window.click("debug-stop-confirm", cx);
                view
            })
            .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.done));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("debug-stop-confirm").is_none());
            assert!(window.find("debug-stop-cancel").visible());
        })
        .unwrap();
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(line, _)| line.starts_with("GET ") && !line.contains("/exec"))
        );
    }

    #[::core::prelude::v1::test]
    fn legacy_stop_failure_keeps_the_dialog_open_without_claiming_success() {
        let cx = &mut support::context();
        let object = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web","namespace":"default","uid":"one"},"spec":{"containers":[{"name":"app","image":"fixture"}],"ephemeralContainers":[{"name":"debugger","image":"busybox","command":["sh","-c","while true; do sleep 3600; done"]}]},"status":{"phase":"Running","ephemeralContainerStatuses":[{"name":"debugger","image":"busybox","imageID":"fixture","ready":false,"restartCount":0,"state":{"running":{}}}]}});
        let (fixture, session) = support::fixture(cx, "debug-stop-legacy", vec![object.clone()]);
        let window = support::window(cx);
        let view = cx
            .update_window(window, |_, window, cx| {
                let view = open(
                    session,
                    Arc::new(serde_json::from_value(object).unwrap()),
                    "debugger".into(),
                    window,
                    cx,
                );
                window.render_frame(cx);
                window.click("debug-stop-confirm", cx);
                view
            })
            .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| !view.busy && view.error.is_some())
        });
        assert!(!view.read_with(cx, |view, _| view.done));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("debug-stop-confirm").visible());
        })
        .unwrap();
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(line, _)| !line.contains("/exec"))
        );
    }
}
