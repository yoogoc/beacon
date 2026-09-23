//! `cargo run -p beacon-kube --example watch -- [context] [namespace]`
//!
//! Lists and follows pods the way the application does, without a window. The
//! rule that `beacon-kube` never depends on GPUI is what makes this possible,
//! and this is the cheapest way to check a change against a real cluster:
//! whatever this prints is what the table will show.

use std::{collections::BTreeMap, sync::Arc};

use beacon_columns::{Cell, ColumnSet};
use beacon_kube::{
    ClusterId, ClusterSession, Delta, ObjectRef, ResourceStore, WatchKey, config::Contexts,
    resources,
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

    let mut args = std::env::args().skip(1);
    let context = match args.next() {
        Some(name) => ClusterId::new(name),
        None => Contexts::load()?
            .current()
            .map(|entry| entry.id.clone())
            .ok_or_else(|| anyhow::anyhow!("no current context; pass one as an argument"))?,
    };
    let namespace = args.next();

    let session = ClusterSession::connect(context).await?;
    println!("connected to {} at {}", session.id(), session.server());

    let key = WatchKey::all(resources::pod()).in_namespace(namespace);
    let columns = ColumnSet::for_kind("", "Pod", key.namespace.is_none());
    let mut subscription = session.subscribe(key);

    let mut store = ResourceStore::new();
    let mut listed = false;

    while let Some(batch) = subscription.next().await {
        // The first batch of a fresh watch is an empty Reset from the registry;
        // the real list arrives right behind it.
        let was_relisted = batch.iter().any(|delta| matches!(delta, Delta::Reset(_)));
        let changes = describe(&batch);
        store.apply_batch(batch);

        if was_relisted && !listed && !store.is_empty() {
            listed = true;
            print_table(&columns, &store);
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
