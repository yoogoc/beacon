//! Binary-safe exec without a PTY. Retry only a rejected protocol upgrade.
use crate::{ClusterSession, files::Target};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, api::AttachParams};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) type Reader = Box<dyn AsyncRead + Unpin + Send>;
pub(crate) struct Input {
    pub reader: Reader,
    pub size: u64,
}
pub(crate) type Writer = Box<dyn AsyncWrite + Unpin + Send>;
pub(crate) const LIMIT: u64 = 4 * 1024 * 1024;

pub(crate) async fn copy(
    mut input: impl AsyncRead + Unpin,
    output: &mut (impl AsyncWrite + Unpin + ?Sized),
    limit: u64,
    progress: Option<&AtomicU64>,
) -> Result<u64, String> {
    let mut count = 0;
    // Keep transfer buffers off Tokio worker stacks: several concurrent file
    // futures can be nested inside one workspace operation.
    let mut buffer = vec![0; 65536].into_boxed_slice();
    loop {
        let n = input.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > limit {
            return Err("Output exceeds the browsing limit. Download the file instead, or open a smaller directory.".into());
        }
        output
            .write_all(&buffer[..n])
            .await
            .map_err(|e| e.to_string())?;
        if let Some(progress) = progress {
            progress.store(count, Ordering::Relaxed);
        }
    }
    output.flush().await.map_err(|e| e.to_string())?;
    Ok(count)
}

pub(crate) async fn run(
    session: &ClusterSession,
    target: &Target,
    argv: &[String],
    input: Option<Input>,
    output: &mut Writer,
    limit: u64,
    progress: &Arc<AtomicU64>,
) -> Result<u64, String> {
    let api: Api<Pod> = Api::namespaced(session.client().clone(), &target.namespace);
    let params = AttachParams::default()
        .container(&target.container)
        .stdin(input.is_some())
        .stdout(true)
        .stderr(true)
        .tty(false);
    let connected = tokio::time::timeout(
        Duration::from_secs(30),
        api.exec(&target.pod, argv.to_vec(), &params),
    )
    .await
    .map_err(|_| "Connecting to the container timed out.".to_string())?;
    // Keep the input unopened by the transport until negotiation succeeds.
    let mut process = match connected {
        Ok(process) => process,
        Err(error) if crate::terminal::can_fallback(&error) => {
            return kubectl(session, target, argv, input, output, limit, progress).await;
        }
        Err(error) => return Err(crate::error::diagnose(&error)),
    };
    let stdin = process.stdin();
    let stdout = process.stdout().ok_or("Exec has no stdout channel")?;
    let stderr = process.stderr().ok_or("Exec has no stderr channel")?;
    let status = process.take_status().ok_or("Exec has no status channel")?;
    let work = async {
        // Keep stdin open until the server exits. Our writer reads an exact
        // byte count, so it needs no EOF; v4 WebSockets cannot half-close stdin.
        let upload = async {
            let mut stdin = stdin;
            if let (Some(input), Some(writer)) = (input, stdin.as_mut()) {
                let count = copy(
                    input.reader.take(input.size),
                    writer,
                    u64::MAX,
                    Some(progress),
                )
                .await?;
                if count != input.size {
                    return Err("Local input ended before the complete file was transferred; the original was preserved.".into());
                }
                writer.flush().await.map_err(|e| e.to_string())?;
            }
            Ok::<_, String>(stdin)
        };
        let mut errors = Vec::new();
        let (count, _, held_stdin) = tokio::try_join!(
            copy(
                stdout,
                output.as_mut(),
                limit,
                (limit != LIMIT).then_some(progress.as_ref())
            ),
            copy(stderr, &mut errors, 65536, None),
            upload
        )?;
        let status = status
            .await
            .ok_or("Container closed without an exit status")?;
        if status.status.as_deref() != Some("Success") {
            return Err(message(
                &errors,
                status
                    .message
                    .as_deref()
                    .unwrap_or("Container command failed"),
            ));
        }
        drop(held_stdin);
        process.join().await.map_err(|e| e.to_string())?;
        Ok(count)
    };
    tokio::time::timeout(Duration::from_secs(3600), work)
        .await
        .map_err(|_| "File operation exceeded one hour; cancelled.".to_string())?
}

fn message(stderr: &[u8], fallback: &str) -> String {
    let text = String::from_utf8_lossy(stderr).trim().to_owned();
    if text.is_empty() {
        fallback.into()
    } else {
        text
    }
}

fn command(
    session: &ClusterSession,
    target: &Target,
    argv: &[String],
    stdin: bool,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(session.file_program());
    command.args([
        "--context",
        session.id().as_str(),
        "--namespace",
        &target.namespace,
        "--request-timeout=0",
        "exec",
        &target.pod,
        "--container",
        &target.container,
    ]);
    if stdin {
        command.arg("-i");
    }
    if let Some(config) = &session.file_transport().config {
        command.arg("--kubeconfig").arg(config.path());
    }
    command
        .arg("--")
        .args(argv)
        .env("KUBECTL_REMOTE_COMMAND_WEBSOCKETS", "false");
    for (name, value) in session.file_transport().environment() {
        command.env(name, value);
    }
    command
        .stdin(if stdin {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn kubectl(
    session: &ClusterSession,
    target: &Target,
    argv: &[String],
    input: Option<Input>,
    output: &mut Writer,
    limit: u64,
    progress: &Arc<AtomicU64>,
) -> Result<u64, String> {
    let mut child = command(session, target, argv, input.is_some())
        .spawn()
        .map_err(|e| {
            format!("File transfer requires kubectl for this cluster. Could not start kubectl: {e}")
        })?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or("kubectl has no stdout")?;
    let stderr = child.stderr.take().ok_or("kubectl has no stderr")?;
    let work = async {
        let uploading = async {
            if let (Some(input), Some(mut stdin)) = (input, stdin) {
                let count = copy(
                    input.reader.take(input.size),
                    &mut stdin,
                    u64::MAX,
                    Some(progress),
                )
                .await?;
                if count != input.size {
                    return Err("Local input ended before the complete file was transferred; the original was preserved.".into());
                }
                stdin.shutdown().await.map_err(|e| e.to_string())?;
            }
            Ok::<(), String>(())
        };
        let upload = async { Ok::<_, String>(uploading.await.err()) };
        let mut errors = Vec::new();
        let (count, _, upload_error) = tokio::try_join!(
            copy(stdout, output.as_mut(), limit, Some(progress)),
            copy(stderr, &mut errors, 65536, None),
            upload
        )?;
        let status = child.wait().await.map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(message(&errors, &format!("kubectl exited with {status}")));
        }
        if let Some(error) = upload_error {
            return Err(error);
        }
        Ok(count)
    };
    tokio::time::timeout(Duration::from_secs(3600), work)
        .await
        .map_err(|_| "File operation exceeded one hour; cancelled.".to_string())?
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ClusterId;
    use tokio::io::AsyncBufReadExt as _;

    // A v4 remote-command server proves uploads do not depend on stdin EOF.
    const SERVER: &str = r#"
import http.server,sys,hashlib,base64,json,struct,urllib.parse
class Server(http.server.BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def log_message(self,*args): pass
    def do_GET(self):
        command=urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)['command']
        key=self.headers['Sec-WebSocket-Key']
        accept=base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        self.send_response(101); self.send_header('Upgrade','websocket'); self.send_header('Connection','Upgrade'); self.send_header('Sec-WebSocket-Accept',accept); self.send_header('Sec-WebSocket-Protocol','v4.channel.k8s.io'); self.end_headers()
        def exact(n):
            b=b''
            while len(b)<n:
                part=self.rfile.read(n-len(b))
                if not part: raise EOFError()
                b+=part
            return b
        def send(channel,b):
            b=bytes([channel])+b; n=len(b)
            length=bytes([n]) if n<126 else bytes([126])+struct.pack('!H',n) if n<65536 else bytes([127])+struct.pack('!Q',n)
            self.wfile.write(bytes([130])+length+b); self.wfile.flush()
        if command[0]=='echo-stdin':
            expected=int(command[1]); payload=bytearray()
            while len(payload)<expected:
                a,b=exact(2); n=b&127
                if n==126: n=struct.unpack('!H',exact(2))[0]
                elif n==127: n=struct.unpack('!Q',exact(8))[0]
                mask=exact(4) if b&128 else bytes(4)
                packet=bytes(c^mask[i%4] for i,c in enumerate(exact(n)))
                if a&15==8 or packet[:1]==bytes([255]): raise AssertionError('stdin closed before remote exit')
                if packet[:1]==bytes([0]): payload.extend(packet[1:])
            send(1,bytes(payload))
        elif command[0]=='fail':
            send(2,b'permission denied by fixture')
        else: send(1,bytes(range(256))*int(command[1]))
        status={'apiVersion':'v1','kind':'Status','status':'Failure' if command[0]=='fail' else 'Success'}
        if command[0]=='fail': status['reason']='NonZeroExitCode'
        send(3,json.dumps(status).encode())
        self.wfile.write(bytes([136,2,3,232])); self.wfile.flush(); self.close_connection=True
s=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server)
print(s.server_address[1],flush=True); s.serve_forever()
"#;
    async fn native() -> (tokio::process::Child, ClusterSession, Target) {
        let mut child = tokio::process::Command::new("python3")
            .args(["-u", "-c", SERVER])
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut port = String::new();
        tokio::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut port)
            .await
            .unwrap();
        let session = ClusterSession::for_testing(
            ClusterId::new("native-files"),
            format!("http://127.0.0.1:{}", port.trim()),
            vec![],
        );
        let target = Target {
            namespace: "default".into(),
            pod: "file-pod".into(),
            uid: "fixture".into(),
            container: "app".into(),
            container_id: "fixture://app".into(),
        };
        (child, session, target)
    }
    #[tokio::test]
    async fn native_v4_streams_binary_and_exact_length_stdin_and_reports_exit_failure() {
        let (_server, session, target) = native().await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bytes");
        let mut output: Writer = Box::new(tokio::fs::File::create(&path).await.unwrap());
        let bytes = (0..=255).cycle().take(512 * 1024).collect::<Vec<u8>>();
        run(
            &session,
            &target,
            &["echo-stdin".into(), bytes.len().to_string()],
            Some(Input {
                reader: Box::new(std::io::Cursor::new(bytes.clone())),
                size: bytes.len() as u64,
            }),
            &mut output,
            u64::MAX,
            &Arc::new(AtomicU64::new(0)),
        )
        .await
        .unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), bytes);
        let mut output: Writer = Box::new(tokio::io::sink());
        let count = run(
            &session,
            &target,
            &["binary-output".into(), "12288".into()],
            None,
            &mut output,
            u64::MAX,
            &Arc::new(AtomicU64::new(0)),
        )
        .await
        .unwrap();
        assert_eq!(count, 3 * 1024 * 1024);
        let error = run(
            &session,
            &target,
            &["fail".into()],
            None,
            &mut output,
            u64::MAX,
            &Arc::new(AtomicU64::new(0)),
        )
        .await
        .unwrap_err();
        assert!(error.contains("permission denied"));
        assert!(
            run(
                &session,
                &target,
                &["binary-output".into(), "12288".into()],
                None,
                &mut output,
                1024,
                &Arc::new(AtomicU64::new(0))
            )
            .await
            .unwrap_err()
            .contains("limit")
        );
    }
    #[tokio::test]
    async fn cancellation_kills_the_fallback_and_removes_incomplete_downloads() {
        use std::os::unix::fs::PermissionsExt as _;
        let files = crate::test_support::FileFixture::new();
        let fixture = crate::test_support::Fixture::start(vec![files.pod()]).await;
        fixture.refuse_exec();
        let local = tempfile::tempdir().unwrap();
        let pid = local.path().join("pid");
        let program = local.path().join("slow-kubectl");
        std::fs::write(&program,format!("#!/usr/bin/env python3\nimport os,time\nopen({},'w').write(str(os.getpid()))\ntime.sleep(60)\n",serde_json::to_string(pid.to_str().unwrap()).unwrap())).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let session = Arc::new(
            ClusterSession::for_testing(
                ClusterId::new("cancel-files"),
                fixture.url.clone(),
                vec![],
            )
            .with_file_test_program(program),
        );
        let target = Target {
            namespace: "default".into(),
            pod: "file-pod".into(),
            uid: "file-uid".into(),
            container: "app".into(),
            container_id: "fixture://app".into(),
        };
        let browser = crate::files::Browser { session, target };
        let download = local.path().join("result.bin");
        let task = tokio::spawn(async move {
            browser
                .download(
                    &["/file.bin".into()],
                    false,
                    download,
                    Arc::new(AtomicU64::new(0)),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !pid.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let child = std::fs::read_to_string(&pid).unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let alive = tokio::process::Command::new("kill")
                    .args(["-0", child.trim()])
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await
                    .unwrap()
                    .success();
                if !alive {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!local.path().join("result.bin").exists());
        assert_eq!(local.path().read_dir().unwrap().count(), 2);
    }
}
