//! Connections outlive tabs and windows. One in-flight connection per context;
//! disconnect/reconnect notifications reach every workspace.
use crate::bridge::Bridge;
use beacon_kube::{ClusterId, ClusterSession, connection::ConnectionOptions};
use gpui_kit::*;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub(crate) struct SharedConnections(pub Entity<Connections>);
impl Global for SharedConnections {}

#[derive(Clone)]
pub(crate) struct Changed(pub ClusterId);

#[derive(Default)]
pub(crate) struct Connections {
    pub sessions: HashMap<ClusterId, Arc<ClusterSession>>,
    pub disconnected: HashSet<ClusterId>,
    pub errors: HashMap<ClusterId, String>,
    epochs: HashMap<ClusterId, u64>,
    pending: HashMap<ClusterId, Task<()>>,
}
impl EventEmitter<Changed> for Connections {}

impl Connections {
    pub fn connecting(&self, id: &ClusterId) -> bool {
        self.pending.contains_key(id)
    }

    pub fn disconnect(&mut self, id: &ClusterId, cx: &mut Context<Self>) {
        *self.epochs.entry(id.clone()).or_default() += 1;
        self.pending.remove(id);
        self.errors.remove(id);
        self.disconnected.insert(id.clone());
        if let Some(session) = self.sessions.remove(id) {
            session.disconnect();
        }
        tracing::info!(context = %id, "manually disconnected cluster in all workspaces");
        cx.emit(Changed(id.clone()));
        cx.notify();
    }

    pub fn connect(&mut self, id: ClusterId, options: ConnectionOptions, cx: &mut Context<Self>) {
        if self.sessions.contains_key(&id)
            || self.pending.contains_key(&id)
            || self.disconnected.contains(&id)
        {
            return;
        }
        self.errors.remove(&id);
        tracing::info!(context = %id, "connecting shared cluster session");
        let epoch = *self.epochs.entry(id.clone()).or_default();
        let target = id.clone();
        let connecting = Bridge::global(cx)
            .run_cancellable(async move { ClusterSession::connect_with(target, options).await });
        let task_id = id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = connecting.result().await;
            let _ = this.update(cx, |state, cx| {
                if state.epochs.get(&task_id) != Some(&epoch)
                    || state.disconnected.contains(&task_id)
                {
                    return;
                }
                state.pending.remove(&task_id);
                match result {
                    Ok(Ok(session)) => {
                        state.sessions.insert(task_id.clone(), Arc::new(session));
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(context = %task_id, %error, "could not connect");
                        state.errors.insert(task_id.clone(), error.to_string());
                    }
                    Err(error) => {
                        tracing::error!(context = %task_id, %error, "the connect task failed");
                        state.errors.insert(task_id.clone(), error.to_string());
                    }
                }
                cx.emit(Changed(task_id.clone()));
                cx.notify();
            });
        });
        self.pending.insert(id.clone(), task);
        cx.emit(Changed(id));
        cx.notify();
    }
}
