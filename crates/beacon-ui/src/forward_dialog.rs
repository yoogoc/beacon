//! Configure a loopback listener before opening it; presets never auto-connect.
use crate::{bridge::Bridge, copyable_text::copyable_text};
use beacon_kube::{
    ClusterSession,
    forward::{Resolved, Target},
};
use gpui_kit::{
    component::{
        ActiveTheme as _, Disableable as _, Sizable as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder as _,
    *,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Preset {
    pub cluster: String,
    pub target: Target,
    pub local_port: u16,
}
pub(crate) enum Event {
    Closed,
    Started,
}
pub(crate) struct ForwardDialog {
    session: Arc<ClusterSession>,
    target: Target,
    local: Entity<InputState>,
    remote: Entity<InputState>,
    save: bool,
    busy: bool,
    checking: bool,
    resolved: Option<Resolved>,
    error: Option<String>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<Event> for ForwardDialog {}
impl ForwardDialog {
    pub fn new(
        session: Arc<ClusterSession>,
        target: Target,
        local_port: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let make = |value: String, window: &mut Window, cx: &mut App| {
            cx.new(|cx| {
                let mut field = InputState::new(window, cx);
                field.set_value(value, window, cx);
                field
            })
        };
        let local = make(local_port.to_string(), window, cx);
        let remote = make(
            if target.port == 0 {
                String::new()
            } else {
                target.port.to_string()
            },
            window,
            cx,
        );
        let subscription = cx.subscribe(&remote, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view._task = None;
                view.checking = false;
                view.resolved = None;
                view.error = None;
                cx.notify();
            }
        });
        Self {
            session,
            target,
            local,
            remote,
            save: false,
            busy: false,
            checking: false,
            resolved: None,
            error: None,
            _task: None,
            _subscriptions: vec![subscription],
        }
    }
    #[cfg(all(test, feature = "ui-tests"))]
    pub(crate) fn ready(&self) -> bool {
        self.resolved.is_some() && !self.checking
    }
    pub fn busy(&self) -> bool {
        self.busy
    }
    fn values(&self, cx: &App) -> Result<(Target, u16), String> {
        let local = self
            .local
            .read(cx)
            .value()
            .trim()
            .parse::<u16>()
            .map_err(|_| "Local port must be 0–65535; 0 chooses a free port.".to_string())?;
        let mut target = self.target.clone();
        target.port = self
            .remote
            .read(cx)
            .value()
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or("Remote port must be 1–65535.")?;
        target.validate().map_err(|error| error.user_message())?;
        Ok((target, local))
    }
    fn resolve(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (target, _) = match self.values(cx) {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.checking = true;
        self.resolved = None;
        self.error = None;
        let session = self.session.clone();
        let task=Bridge::global(cx).run_cancellable(async move {
            let resolved=tokio::time::timeout(std::time::Duration::from_secs(15),target.resolve(session.client().clone())).await.map_err(|_|"Target lookup timed out.".to_string())?.map_err(|error|error.user_message())?;
            let rules=session.rules(Some(target.namespace.clone())).await;
            if !rules.allows_named("get","","pods/portforward",&resolved.pod) { return Err(format!("Forbidden: cannot get resource \"pods/portforward\" in API group \"\" in the namespace \"{}\".", target.namespace)); }
            Ok(resolved)
        });
        self._task = Some(cx.spawn_in(window, async move |view, cx| {
            let result = task.result().await;
            let _ = view.update(cx, |view, cx| {
                view.checking = false;
                match result {
                    Ok(Ok(route)) => view.resolved = Some(route),
                    Ok(Err(error)) => view.error = Some(error),
                    Err(error) => view.error = Some(error.to_string()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.resolved.is_none() {
            return;
        }
        let (target, local) = match self.values(cx) {
            Ok(values) => values,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        let session = self.session.clone();
        let task = Bridge::global(cx).run_cancellable(async move {
            session
                .forward_target(target, local)
                .await
                .map_err(|error| error.user_message())
        });
        self._task=Some(cx.spawn_in(window,async move|view,cx|{
            let result=task.result().await;
            let _=view.update(cx,|view,cx|{view.busy=false;match result {
                Ok(Ok(forward))=>{
                    if view.save {
                        let preset=Preset {cluster:view.session.id().as_str().into(),target:forward.target,local_port:forward.local_port};
                        let mut presets=crate::settings::store(cx).read(cx).preferences.forwards.clone();
                        if !presets.contains(&preset) {presets.push(preset);}
                        if let Err(error)=crate::settings::save_forwards(presets,cx) {
                            view.session.close_forward(forward.id);
                            view.error=Some(crate::i18n::message("messages.could_not_save_the_preset_error_the_listener_was_stopped_retry_without_saving_or_fix_the_config_folder", &[("error", (error).to_string())]).to_string());cx.notify();return;
                        }
                    }
                    cx.emit(Event::Started);
                },
                Ok(Err(error))=>view.error=Some(error),Err(error)=>view.error=Some(error.to_string())
            };cx.notify();});
        }));
        cx.notify();
    }
}
impl Render for ForwardDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().id("forward-dialog").test_support().w(px(560.)).max_w_full().max_h_full().overflow_y_scroll().p_5().gap_4().rounded_lg().border_1().border_color(cx.theme().border).bg(cx.theme().background).text_color(cx.theme().foreground)
            .child(div().text_lg().child(crate::i18n::text("Port forward")))
            .child(copyable_text("forward-target",format!("{} · {}/{}",self.target.kind.label(),self.target.namespace,self.target.name)))
            .child(h_flex().gap_4()
                .child(v_flex().flex_1().gap_1().child(div().text_sm().child(crate::i18n::text("Local port"))).child(Input::new(&self.local).disabled(self.busy)).child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text("127.0.0.1 · 0 chooses a free port"))))
                .child(v_flex().flex_1().gap_1().child(div().text_sm().child(crate::i18n::text(if self.target.kind==beacon_kube::forward::TargetKind::Service {"Service port (TCP)"} else {"Container port (TCP)"}))).child(Input::new(&self.remote).disabled(self.busy))))
            .child(Button::new("forward-resolve").small().outline().label(crate::i18n::text(if self.checking {"Finding Pod…"} else {"Check target"})).disabled(self.busy||self.checking).on_click(cx.listener(|view,_,window,cx|view.resolve(window,cx))))
            .when_some(self.resolved.as_ref(),|view,route|view.child(div().text_sm().child(copyable_text("forward-resolved",crate::i18n::message("messages.backend", &[("value0", (route.pod).to_string()), ("value1", (route.port).to_string())]).to_string()))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text("Service and Deployment forwards keep the local port and select a ready Pod after replacement. Existing TCP connections need to reconnect.")))
            .child(crate::selection::checkbox("forward-save").label(crate::i18n::text("Save this forward for later")).checked(self.save).disabled(self.busy).on_click(cx.listener(|view,value,_,cx|{view.save = *value;cx.notify();})))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text("Saved forwards only start when you choose Restore in Port Forwards.")))
            .when_some(self.error.as_ref(),|view,error|view.child(div().id("forward-error").test_support().text_sm().text_color(cx.theme().danger).child(copyable_text("forward-error-text",error.clone()))).children(crate::permissions::error_link("forward-permissions",self.session.clone(),error,beacon_kube::rbac::Query {verb:"get".into(),resource:"pods".into(),subresource:"portforward".into(),namespace:Some(self.target.namespace.clone()),..Default::default()})))
            .child(h_flex().justify_end().gap_2()
                .child(Button::new("forward-cancel").small().ghost().label(crate::i18n::text("Cancel")).disabled(self.busy).on_click(cx.listener(|_,_,_,cx|cx.emit(Event::Closed))))
                .child(Button::new("forward-start").small().primary().label(crate::i18n::text(if self.busy {"Starting…"} else {"Start forwarding"})).disabled(self.busy||self.resolved.is_none()||self.checking).on_click(cx.listener(|view,_,window,cx|view.start(window,cx)))))
    }
}

pub(crate) fn ports(kind: &str, data: &serde_json::Value) -> Vec<u16> {
    let mut ports = std::collections::BTreeSet::new();
    if kind == "Service" {
        for port in data
            .pointer("/spec/ports")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if port["protocol"].as_str().unwrap_or("TCP") == "TCP"
                && let Some(port) = port["port"]
                    .as_u64()
                    .and_then(|n| u16::try_from(n).ok())
                    .filter(|n| *n > 0)
            {
                ports.insert(port);
            }
        }
    } else {
        let path = if kind == "Deployment" {
            "/spec/template/spec/containers"
        } else {
            "/spec/containers"
        };
        for port in data
            .pointer(path)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|container| container["ports"].as_array().into_iter().flatten())
        {
            if port["protocol"].as_str().unwrap_or("TCP") == "TCP"
                && let Some(port) = port["containerPort"]
                    .as_u64()
                    .and_then(|n| u16::try_from(n).ok())
                    .filter(|n| *n > 0)
            {
                ports.insert(port);
            }
        }
    }
    ports.into_iter().collect()
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests;
