use super::*;
use crate::{
    copyable_text::copyable_text,
    forward_dialog::{Event, ForwardDialog, Preset},
};
use beacon_kube::forward::{Target, TargetKind};
impl ClusterView {
    pub(crate) fn start_forward_for(
        &mut self,
        target: ObjectRef,
        remote_port: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(kind) = self
            .kind
            .as_ref()
            .and_then(|kind| TargetKind::from_resource(&kind.resource.group, &kind.resource.kind))
        else {
            return;
        };
        let Some(namespace) = target.namespace else {
            return;
        };
        self.configure_forward(
            Target {
                kind,
                namespace,
                name: target.name,
                port: remote_port,
            },
            0,
            window,
            cx,
        );
    }
    fn configure_forward(
        &mut self,
        target: Target,
        local: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.forward_dialog.is_some() {
            return;
        }
        let dialog =
            cx.new(|cx| ForwardDialog::new(self.session.clone(), target, local, window, cx));
        self._subscriptions
            .push(cx.subscribe(&dialog, |view, _, event: &Event, cx| {
                view.forward_dialog = None;
                if matches!(event, Event::Started) {
                    view.mode = Mode::Forwards;
                    cx.emit(NavigationChanged);
                }
                cx.notify();
            }));
        self.forward_dialog = Some(dialog);
        cx.notify();
    }
    fn save_forward(&mut self, preset: Preset, cx: &mut Context<Self>) {
        let mut presets = crate::settings::store(cx)
            .read(cx)
            .preferences
            .forwards
            .clone();
        if !presets.contains(&preset) {
            presets.push(preset);
        }
        if let Err(error) = crate::settings::save_forwards(presets, cx) {
            self.outcome = Some(Outcome::Failed(error));
        }
        cx.notify();
    }
    pub(super) fn render_forwards(&self, cx: &mut Context<Self>) -> AnyElement {
        let forwards = self.session.forwards();
        let presets: Vec<_> = crate::settings::store(cx)
            .read(cx)
            .preferences
            .forwards
            .iter()
            .filter(|p| p.cluster == self.session.id().as_str())
            .cloned()
            .collect();
        v_flex().id("forwards").size_full().overflow_y_scroll().p_4().gap_3()
            .child(h_flex().gap_2().child(div().text_lg().child(crate::i18n::text("Port Forwards"))).child(div().text_sm().text_color(cx.theme().muted_foreground).child(format!("{} active",forwards.len()))))
            .when(forwards.is_empty(),|view|view.child(div().text_sm().text_color(cx.theme().muted_foreground).child(crate::i18n::text("No active forwards. Right-click a Pod, Service or Deployment to configure one, or restore a saved forward below."))))
            .children(forwards.into_iter().map(|forward| {
                let id=forward.id;
                let preset=Preset {cluster:self.session.id().as_str().into(),target:forward.target.clone(),local_port:forward.local_port};
                let saved=presets.contains(&preset);
                v_flex().p_3().gap_2().rounded_md().border_1().border_color(cx.theme().border)
                    .child(h_flex().gap_3().items_center()
                        .child(v_flex().flex_1().min_w_0().gap_1()
                            .child(div().text_sm().font_family("monospace").child(copyable_text(("forward-address",id),forward.address())))
                            .child(div().text_sm().child(copyable_text(("forward-logical-target",id),forward.target.describe())))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} · Pod {}:{} · {} connections",if forward.reconnecting.is_some(){"Waiting for target"}else{"Listening"},forward.pod,forward.remote_port,forward.connections))))
                        .child(Button::new(("save-forward",id)).small().ghost().label(crate::i18n::text(if saved {"Saved"}else{"Save"})).disabled(saved).on_click(cx.listener(move|view,_,_,cx|view.save_forward(preset.clone(),cx))))
                        .child(Button::new(("close-forward",id)).small().outline().label(crate::i18n::text("Stop")).on_click(cx.listener(move|view,_,_,cx|view.close_forward(id,cx)))))
                    .when_some(forward.reconnecting,|view,error|view.child(div().text_xs().text_color(cx.theme().warning).child(copyable_text(("forward-reconnect",id),crate::i18n::message("messages.retrying_target_error", &[("error", (error).to_string())]).to_string()))))
                    .when_some(forward.failure,|view,error|view.child(div().text_xs().text_color(cx.theme().danger).child(copyable_text(("forward-failure",id),error.clone()))).children(crate::permissions::error_link(("forward-permission",id),self.session.clone(),&error,beacon_kube::rbac::Query {verb:"get".into(),resource:"pods".into(),subresource:"portforward".into(),namespace:Some(forward.namespace),name:Some(forward.pod),..Default::default()})))
            }))
            .child(div().mt_3().text_sm().child(crate::i18n::text("Saved forwards")))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text("Restore opens the configuration for review. Saved forwards never start automatically.")))
            .when(presets.is_empty(),|view|view.child(div().text_sm().text_color(cx.theme().muted_foreground).child(crate::i18n::text("No saved forwards for this cluster."))))
            .children(presets.into_iter().enumerate().map(|(index,preset)| {
                let remove=preset.clone();let active=self.session.forwards().iter().any(|f|f.target==preset.target&&f.local_port==preset.local_port);
                h_flex().id(("forward-preset",index)).test_support().p_3().gap_3().items_center().rounded_md().border_1().border_color(cx.theme().border)
                    .child(v_flex().flex_1().min_w_0().gap_1().child(div().text_sm().child(copyable_text(("forward-preset-target",index),preset.target.describe()))).child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::message("messages.local_127_0_0_1", &[("value0", (preset.local_port).to_string())]).to_string())))
                    .child(Button::new(("restore-forward",index)).small().outline().label(crate::i18n::text(if active {"Active"}else{"Restore"})).disabled(active).on_click(cx.listener(move|view,_,window,cx|view.configure_forward(preset.target.clone(),preset.local_port,window,cx))))
                    .child(Button::new(("remove-forward",index)).small().ghost().label(crate::i18n::text("Remove")).on_click(cx.listener(move|view,_,_,cx|{
                        let mut presets=crate::settings::store(cx).read(cx).preferences.forwards.clone();presets.retain(|p|p!=&remove);
                        if let Err(error)=crate::settings::save_forwards(presets,cx){view.outcome=Some(Outcome::Failed(error));}cx.notify();
                    })))
            })).into_any_element()
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    #[::core::prelude::v1::test]
    fn forward_presets_are_cluster_scoped_and_restore_requires_review_then_stop_releases_listener()
    {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let (_, session) = support::fixture(cx, "presets-empty", vec![]);
        drop(session);
        let (_fixture, session) = support::fixture(
            cx,
            "presets",
            vec![
                json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web","namespace":"default","uid":"p1"},"spec":{"containers":[{"name":"app","image":"fixture"}]},"status":{"phase":"Running"}}),
            ],
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let preset = Preset {
            cluster: "presets".into(),
            target: Target {
                kind: TargetKind::Pod,
                namespace: "default".into(),
                name: "web".into(),
                port: 8080,
            },
            local_port: port,
        };
        cx.update(|cx| {
            crate::settings::save_forwards(
                vec![
                    preset.clone(),
                    Preset {
                        cluster: "other".into(),
                        ..preset
                    },
                ],
                cx,
            )
            .unwrap()
        });
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    ClusterView::new(
                        session.clone(),
                        Some("default".into()),
                        None,
                        None,
                        false,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.show_mode(Mode::Forwards, window, cx));
            window.render_frame(cx);
        })
        .unwrap();
        assert!(session.forwards().is_empty());
        cx.update_window(window, |_, window, cx| {
            window.click(("restore-forward", 0usize), cx)
        })
        .unwrap();
        assert!(view.read_with(cx, |view, _| view.forward_dialog.is_some()));
        assert!(session.forwards().is_empty());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("forward-resolve", cx);
        })
        .unwrap();
        // Poll the actual control rather than directly calling the start handler.
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| {
                view.forward_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.read(cx).ready())
            })
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("forward-start", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| view.forward_dialog.is_none())
        });
        let active = session.forwards();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].local_port, port);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("close-forward", active[0].id), cx);
        })
        .unwrap();
        assert!(session.forwards().is_empty());
    }
}
