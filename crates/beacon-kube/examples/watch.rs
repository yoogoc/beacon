//! `cargo run -p beacon-kube --example watch -- [--context NAME] [--once] [Kind] [namespace]`
//! `cargo run -p beacon-kube --example watch -- --kinds`
//!
//! Lists and follows any kind the cluster serves, with no window at all,
//! printing exactly the columns the table renders. The rule that `beacon-kube`
//! never depends on GPUI is what makes this possible, and it is the cheapest
//! way to check a change: diff it against `kubectl get <kind>`.

use std::{collections::BTreeMap, sync::Arc};

use beacon_columns::{Cell, ColumnSet};
use beacon_kube::{
    ClusterId, ClusterSession, Delta, ObjectRef, ResourceStore, WatchKey, config::Contexts,
};
use futures::StreamExt as _;
use k8s_openapi::jiff::Timestamp;
use kube::api::DynamicObject;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("beacon_kube=debug")),
        )
        .with_writer(std::io::stderr)
        .init();

    let arguments = Arguments::parse()?;

    let context = match arguments.context {
        Some(name) => ClusterId::new(name),
        None => Contexts::load()?
            .current()
            .map(|entry| entry.id.clone())
            .ok_or_else(|| anyhow::anyhow!("no current context; pass --context"))?,
    };

    let session = Arc::new(ClusterSession::connect(context).await?);
    println!("connected to {} at {}", session.id(), session.server());

    if arguments.list_kinds {
        for kind in session.discovery().kinds() {
            println!(
                "  {:<52} {}",
                kind.display_name(),
                if kind.namespaced {
                    "namespaced"
                } else {
                    "cluster"
                }
            );
        }
        return Ok(());
    }

    let kind = session
        .discovery()
        .find(&arguments.kind)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{:?} is not served by this cluster; try --kinds",
                arguments.kind
            )
        })?
        .clone();

    // A cluster-scoped kind ignores the namespace, which is also what stops the
    // table growing a Namespace column it would never fill.
    let namespace = arguments.namespace.filter(|_| kind.namespaced);
    let key = WatchKey::all(kind.resource.clone()).in_namespace(namespace);
    let show_namespace = kind.namespaced && key.namespace.is_none();

    let printer_columns = session.clone().printer_columns(kind.resource.clone()).await;
    let columns = ColumnSet::resolve(
        &kind.resource.group,
        &kind.resource.kind,
        show_namespace,
        printer_columns.as_deref(),
    );

    println!(
        "{} at {}, columns from {}",
        kind.display_name(),
        kind.resource.api_version,
        if printer_columns.is_some() {
            "the CRD"
        } else {
            "the built-in table or the fallback"
        }
    );

    let mut subscription = session.subscribe(key);
    let mut store = ResourceStore::new();
    let mut listed = false;

    while let Some(batch) = subscription.next().await {
        // A `Reset` means "this is everything", including when it is empty.
        let was_relisted = batch.iter().any(|delta| matches!(delta, Delta::Reset(_)));
        let changes = describe(&batch);
        store.apply_batch(batch);

        if was_relisted && !listed {
            listed = true;
            print_table(&columns, &store);
            if arguments.once {
                return Ok(());
            }
            println!("\nwatching for changes; ^C to stop");
            continue;
        }

        if listed {
            for change in changes {
                println!("{change}");
            }
        }
    }

    Ok(())
}

struct Arguments {
    context: Option<String>,
    kind: String,
    namespace: Option<String>,
    list_kinds: bool,
    /// Print the first list and stop, for scripting a comparison against
    /// `kubectl get`.
    once: bool,
}

impl Arguments {
    fn parse() -> anyhow::Result<Self> {
        let mut context = None;
        let mut positional = Vec::new();
        let mut list_kinds = false;
        let mut once = false;

        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--kinds" => list_kinds = true,
                "--once" => once = true,
                "--context" => {
                    context = Some(
                        arguments
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("--context needs a name"))?,
                    );
                }
                other if other.starts_with('-') => {
                    anyhow::bail!("unknown option {other:?}");
                }
                other => positional.push(other.to_string()),
            }
        }

        let mut positional = positional.into_iter();
        Ok(Self {
            context,
            kind: positional.next().unwrap_or_else(|| "Pod".to_string()),
            namespace: positional.next(),
            list_kinds,
            once,
        })
    }
}

fn describe(batch: &[Delta]) -> Vec<String> {
    batch
        .iter()
        .map(|delta| match delta {
            Delta::Reset(objects) => format!("  relisted {} objects", objects.len()),
            Delta::Upsert(object) => format!("  ~ {}", ObjectRef::of(object)),
            Delta::Remove(key) => format!("  - {key}"),
        })
        .collect()
}

fn print_table(columns: &ColumnSet, store: &ResourceStore) {
    let now = Timestamp::now();

    let mut rows: BTreeMap<ObjectRef, &Arc<DynamicObject>> = BTreeMap::new();
    for (key, object) in store.iter() {
        rows.insert(key.clone(), object);
    }

    let mut table = vec![
        columns
            .headers()
            .iter()
            .map(|header| header.to_uppercase())
            .collect::<Vec<_>>(),
    ];

    for object in rows.values() {
        let cell = Cell {
            metadata: &object.metadata,
            data: &object.data,
            now,
        };
        table.push(
            columns
                .columns
                .iter()
                .map(|column| column.resolve(&cell).display().to_string())
                .collect(),
        );
    }

    let widths: Vec<usize> = (0..columns.len())
        .map(|index| {
            table
                .iter()
                .map(|row| row[index].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();

    for row in &table {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(value, width)| format!("{value:<width$}"))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}
