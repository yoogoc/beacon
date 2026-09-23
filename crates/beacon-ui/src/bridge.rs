//! The tokio <-> GPUI bridge.
//!
//! `kube` is built on hyper, which needs a tokio reactor registered on the
//! current thread. GPUI's executor is not tokio, so polling a kube future on a
//! GPUI executor panics with "there is no reactor running".
//!
//! The rule is therefore: one tokio runtime owns every network task, GPUI owns
//! every view, and the two only exchange messages over runtime-agnostic
//! channels. The foreground thread never blocks, and nothing but plain data
//! crosses the boundary.
//!
//! Two shapes cover everything so far. [`Bridge::run`] starts one piece of
//! network work and hands back its result -- connecting, fetching an object.
//! [`drain_into`] feeds a stream of messages into a view; the stream itself is
//! a channel, so the foreground thread can poll it directly, and only its
//! producer lives on the runtime.

use std::{future::Future, sync::Arc};

use futures::{Stream, StreamExt as _};
use gpui_kit::*;

/// Owns the tokio runtime, installed as a GPUI global.
pub struct Bridge {
    runtime: Arc<tokio::runtime::Runtime>,
}

impl Global for Bridge {}

impl Bridge {
    /// Builds the runtime and installs it. Call once, during startup.
    pub fn init(cx: &mut App) -> anyhow::Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("beacon-net")
            // Cluster work is I/O bound, not CPU bound. A handful of threads is
            // plenty, and it keeps memory predictable when several clusters are
            // connected at once.
            .worker_threads(4)
            .build()?;

        cx.set_global(Self {
            runtime: Arc::new(runtime),
        });
        Ok(())
    }

    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// A handle to the runtime, for code that starts its own long-lived task
    /// there -- the terminal's duplex stream, which is neither a one-shot nor
    /// a plain producer.
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// Runs one piece of network work on the runtime.
    ///
    /// The returned handle is an ordinary future: `await` it from a GPUI task
    /// and the result comes back on the foreground thread. The work keeps going
    /// if the handle is dropped, so anything that must stop when a view goes
    /// away belongs in a stream instead.
    pub fn run<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }
}

/// Feeds everything a stream yields into an entity, one `apply` per message.
///
/// The stream is polled on the foreground thread, which is only correct because
/// the streams that cross this boundary are channels -- they have no I/O of
/// their own to drive. `apply` therefore runs on the same thread as rendering,
/// and two properties of the producer are what keep that cheap:
///
/// * **Messages arrive pre-batched.** A first `list` of a large namespace
///   produces thousands of events; one `apply` (and one `cx.notify()`) per event
///   would stall the frame loop for seconds. Producers coalesce on a
///   frame-sized time window before sending.
/// * **The producer stops when the view goes away.** Dropping the returned
///   [`Task`] drops the stream, which is the producer's signal to shut down.
pub fn drain_into<T, S>(
    cx: &mut Context<T>,
    stream: S,
    mut apply: impl FnMut(&mut T, S::Item, &mut Window, &mut Context<T>) + 'static,
    window: &Window,
) -> Task<()>
where
    T: 'static,
    S: Stream + 'static,
{
    cx.spawn_in(window, async move |this, cx| {
        futures::pin_mut!(stream);
        while let Some(message) = stream.next().await {
            // An error here means the entity was dropped. Returning drops the
            // stream, which tells the producer to stop.
            if this
                .update_in(cx, |view, window, cx| apply(view, message, window, cx))
                .is_err()
            {
                break;
            }
        }
    })
}
