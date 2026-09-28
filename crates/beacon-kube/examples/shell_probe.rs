//! Headless reproduction of the GUI's TTY transport.
//! `cargo run -p beacon-kube --example shell_probe -- CONTEXT NAMESPACE POD COMMAND [ARGS...]`

use std::time::Duration;

use beacon_kube::TerminalEvent;
use futures::StreamExt as _;
use kube::{Client, Config, config::KubeConfigOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let context = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing context"))?;
    let namespace = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing namespace"))?;
    let pod = args.next().ok_or_else(|| anyhow::anyhow!("missing pod"))?;
    let command: Vec<_> = args.collect();
    anyhow::ensure!(!command.is_empty(), "missing command");
    let config = Config::from_kubeconfig(&KubeConfigOptions {
        context: Some(context.clone()),
        ..Default::default()
    })
    .await?;
    eprintln!("proxy configured: {}", config.proxy_url.is_some());
    let client = Client::try_from(config)?;
    let (terminal, mut events) = beacon_kube::terminal::attach(
        client,
        &tokio::runtime::Handle::current(),
        context,
        namespace,
        pod,
        None,
        command,
    );
    terminal.resize(100, 28);
    tokio::time::timeout(Duration::from_secs(75), async {
        while let Some(event) = events.next().await {
            match event {
                TerminalEvent::Output(bytes) => {
                    eprintln!("OUTPUT {:?}", String::from_utf8_lossy(&bytes))
                }
                TerminalEvent::Failed(error) => anyhow::bail!(error),
                event => eprintln!("EVENT {event:?}"),
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
