use beacon_updater::{Asset, Client, Format, Proxy, Release, verify_package};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};
use url::Url;

const KEY: &str = include_str!("fixtures/public-key");
const SIGNATURE: &str = include_str!("fixtures/package.sig");
const PACKAGE: &[u8] = include_bytes!("fixtures/package");

async fn server(body: Vec<u8>, hang: bool) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        let _ = socket.read(&mut buffer).await;
        socket
            .write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
            )
            .await
            .unwrap();
        if hang {
            socket.write_all(&body[..1]).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        } else {
            socket.write_all(&body).await.unwrap();
        }
    });
    Url::parse(&format!("http://{address}/package")).unwrap()
}
fn release(url: Url, size: usize) -> Release {
    Release {
        version: "0.2.8".parse().unwrap(),
        notes: String::new(),
        page: "https://github.com/yoogoc/beacon/releases/tag/v0.2.8"
            .parse()
            .unwrap(),
        asset: Asset {
            url,
            signature: SIGNATURE.into(),
            size: size as u64,
            format: Format::App,
        },
    }
}
#[tokio::test]
async fn signed_download_is_verified_and_removed_when_discarded() {
    let cache = tempfile::tempdir().unwrap();
    let client = Client::new(Proxy::Direct, KEY).unwrap();
    let url = server(PACKAGE.to_vec(), false).await;
    let mut progress = Vec::new();
    let ready = client
        .download(
            release(url, PACKAGE.len()),
            cache.path().to_owned(),
            |n, total| progress.push((n, total)),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(&ready.path).unwrap(), PACKAGE);
    assert_eq!(
        progress.last(),
        Some(&(PACKAGE.len() as u64, PACKAGE.len() as u64))
    );
    verify_package(KEY, &ready.path, SIGNATURE).unwrap();
    std::fs::write(&ready.path, b"changed after download").unwrap();
    assert!(verify_package(KEY, &ready.path, SIGNATURE).is_err());
    let path = ready.path.clone();
    drop(ready);
    assert!(!path.exists());
}
#[tokio::test]
async fn tampering_and_incomplete_size_do_not_leave_installable_packages() {
    let cache = tempfile::tempdir().unwrap();
    let client = Client::new(Proxy::Direct, KEY).unwrap();
    let mut bad = PACKAGE.to_vec();
    bad[0] ^= 1;
    for (body, size) in [(bad, PACKAGE.len()), (PACKAGE.to_vec(), PACKAGE.len() + 1)] {
        let url = server(body, false).await;
        assert!(
            client
                .download(release(url, size), cache.path().to_owned(), |_, _| {})
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
    }
}
#[tokio::test]
async fn cancelling_a_stalled_download_removes_its_partial_file() {
    let cache = tempfile::tempdir().unwrap();
    let client = Client::new(Proxy::Direct, KEY).unwrap();
    let url = server(PACKAGE.to_vec(), true).await;
    let path = cache.path().to_owned();
    let task = tokio::spawn(async move {
        client
            .download(release(url, PACKAGE.len()), path, |_, _| {})
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 1);
    task.abort();
    let _ = task.await;
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
}
