use super::*;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};
const SERVER: &str = r#"
import http.server,sys,hashlib,base64,json,struct,urllib.parse,threading
active=0
lock=threading.Lock()
class Server(http.server.BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def log_message(self,*args): pass
    def do_GET(self):
        global active
        if self.path=='/stats': body={'active':active}
        else: body={'apiVersion':'v1','kind':'Pod','metadata':{'name':'echo','namespace':'team','uid':'echo-1'},'spec':{'containers':[{'name':'echo','image':'fixture'}]},'status':{'phase':'Running'}}
        if '/portforward' not in self.path:
            data=json.dumps(body).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data);return
        key=self.headers['Sec-WebSocket-Key'];accept=base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        self.send_response(101);self.send_header('Upgrade','websocket');self.send_header('Connection','Upgrade');self.send_header('Sec-WebSocket-Accept',accept);self.send_header('Sec-WebSocket-Protocol','v4.channel.k8s.io');self.end_headers()
        def exact(n):
            data=b''
            while len(data)<n:
                part=self.rfile.read(n-len(data))
                if not part: raise EOFError()
                data+=part
            return data
        def send(ch,data):
            data=bytes([ch])+data;n=len(data);size=bytes([n]) if n<126 else bytes([126])+struct.pack('!H',n)
            self.wfile.write(bytes([130])+size+data);self.wfile.flush()
        with lock: active+=1
        try:
            send(0,struct.pack('<H',8080));send(1,struct.pack('<H',8080))
            while True:
                a,b=exact(2);n=b&127
                if n==126:n=struct.unpack('!H',exact(2))[0]
                elif n==127:n=struct.unpack('!Q',exact(8))[0]
                mask=exact(4) if b&128 else bytes(4);packet=bytes(c^mask[i%4] for i,c in enumerate(exact(n)))
                if a&15==8:break
                if packet[:1]==bytes([0]):send(0,packet[1:])
        except (EOFError,BrokenPipeError,ConnectionResetError):pass
        finally:
            with lock:active-=1
            self.close_connection=True
s=http.server.ThreadingHTTPServer(('127.0.0.1',0),Server)
print(s.server_address[1],flush=True);s.serve_forever()
"#;
#[tokio::test]
async fn native_forward_copies_bytes_counts_connections_and_stop_closes_remote_tunnels() {
    let mut server = tokio::process::Command::new("python3")
        .args(["-u", "-c", SERVER])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut port = String::new();
    tokio::io::BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut port)
        .await
        .unwrap();
    let url = format!("http://127.0.0.1:{}", port.trim());
    let client = kube::Client::try_from(kube::Config::new(url.parse().unwrap())).unwrap();
    let forwards = Forwards::default();
    let opened = forwards
        .open_target(
            client,
            &tokio::runtime::Handle::current(),
            Target {
                kind: TargetKind::Pod,
                namespace: "team".into(),
                name: "echo".into(),
                port: 8080,
            },
            0,
        )
        .await
        .unwrap();
    let mut stream = tokio::net::TcpStream::connect(opened.address())
        .await
        .unwrap();
    let bytes = (0..=255).cycle().take(4000).collect::<Vec<u8>>();
    stream.write_all(&bytes).await.unwrap();
    let mut received = vec![0; bytes.len()];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, bytes);
    assert_eq!(forwards.list()[0].connections, 1);
    forwards.close(opened.id);
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let stats: serde_json::Value = http
                .get(format!("{url}/stats"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if stats["active"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(forwards.is_empty());
}
