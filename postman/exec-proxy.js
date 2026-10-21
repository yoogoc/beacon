// Postman's HTTP simulation does not forward WebSocket upgrades. This local
// wire adapter forwards REST calls to that simulation and serves kube-rs exec
// frames itself. It never connects to a real Kubernetes cluster.
const http = require('http');
const crypto = require('crypto');
const upstream = process.env.BEACON_DEBUG_HTTP_URL;
if (!upstream || !['127.0.0.1', 'localhost'].includes(new URL(upstream).hostname)) {
  throw new Error('BEACON_DEBUG_HTTP_URL must point to the local Postman simulation');
}
const api = path => upstream.replace(/\/$/, '') + path;
const server = http.createServer(async (req, res) => {
  try {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const response = await fetch(api(req.url), {
      method: req.method,
      headers: { 'Content-Type': req.headers['content-type'] || 'application/json' },
      body: ['GET', 'HEAD'].includes(req.method) ? undefined : Buffer.concat(chunks),
    });
    res.writeHead(response.status, { 'Content-Type': response.headers.get('content-type') || 'application/json' });
    res.end(Buffer.from(await response.arrayBuffer()));
  } catch (error) {
    res.writeHead(502);
    res.end(String(error));
  }
});
server.on('upgrade', async (req, socket) => {
  socket.on('error', () => {});
  socket.on('data', chunk => {
    if ((chunk[0] & 0x0f) === 8) socket.end();
  });
  const url = new URL(req.url, 'http://localhost');
  const match = url.pathname.match(/^\/api\/v1\/namespaces\/default\/pods\/(stop-ready|stop-denied|stop-stuck)\/exec$/);
  const fail = (code, reason) => {
    console.log(JSON.stringify({event:'exec', pod:match?.[1], status:code}));
    const body = JSON.stringify({apiVersion:'v1',kind:'Status',status:'Failure',code,reason,message:reason});
    socket.end(`HTTP/1.1 ${code} ${reason}\r\nContent-Type: application/json\r\nContent-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
  };
  try {
    if (!match) return fail(501, 'NotImplemented');
    if (match[1] === 'stop-denied') return fail(403, 'Forbidden');
    const podPath = `/api/v1/namespaces/default/pods/${match[1]}`;
    const pod = await (await fetch(api(podPath))).json();
    const argv = url.searchParams.getAll('command');
    const container = pod.spec?.ephemeralContainers?.find(item => item.name === url.searchParams.get('container'));
    if (!container || argv[3] !== 'beacon-debug-v1' || argv[4] !== `/tmp/.beacon-debug-${container.name}` || !argv[2]?.includes('beacon-debug-stop-requested')) return fail(422, 'Invalid');
    const protocol = (req.headers['sec-websocket-protocol'] || '').split(',').map(item => item.trim()).find(item => /^v[45]\.channel\.k8s\.io$/.test(item));
    if (!protocol || !req.headers['sec-websocket-key']) return fail(400, 'BadRequest');
    const accept = crypto.createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    const updated = await fetch(api(`/__fixture/stop/${match[1]}`), {method:'POST', headers:{'Content-Type':'application/json'},body:JSON.stringify({container:container.name})});
    if (!updated.ok) return fail(502, 'FixtureError');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\nSec-WebSocket-Protocol: ${protocol}\r\n\r\n`);
    const frame = (channel, text) => {
      const data = Buffer.concat([Buffer.from([channel]), Buffer.from(text)]);
      return Buffer.concat([Buffer.from([0x82, data.length]), data]);
    };
    socket.write(frame(1, 'beacon-debug-stop-requested\n'));
    socket.write(frame(3, JSON.stringify({status:'Success'})));
    socket.write(Buffer.from([0x88, 0x02, 0x03, 0xe8]));
    const cleanup = setTimeout(() => socket.destroy(), 1000);
    cleanup.unref();
    socket.on('close', () => clearTimeout(cleanup));
    console.log(JSON.stringify({event:'exec',pod:match[1],container:container.name,status:101,command:argv}));
  } catch (error) { console.error(error); socket.destroy(); }
});
server.listen(Number(process.env.PORT || 4918), '127.0.0.1', () => console.log('exec wire adapter listening'));
