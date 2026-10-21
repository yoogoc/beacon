// Narrow fixture of the Kubernetes Pod/ephemeralcontainers contract:
// https://kubernetes.io/docs/reference/kubernetes-api/workload-resources/pod-v1/
// State is isolated from every other simulation member. No real cluster is used.
const http = require('http');
const send = (res, code, body) => {
  res.writeHead(code, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify(body));
};
const status = (reason, message, code) => ({apiVersion:'v1', kind:'Status', status:'Failure', reason, message, code});
const server = http.createServer(async (req, res) => {
  // Register stream readers before the first await; simulation routing may
  // forward raw IncomingMessages even with interception enabled.
  const incoming = req.body !== undefined ? Promise.resolve(req.body) : new Promise((resolve, reject) => {
    let raw = '';
    req.on('data', chunk => { raw += chunk; });
    req.on('end', () => { try { resolve(raw ? JSON.parse(raw) : {}); } catch (error) { reject(error); } });
    req.on('error', reject);
  });
  const path = new URL(req.url, 'http://localhost').pathname;
  const stopped = path.match(/^\/__fixture\/stop\/(stop-ready|stop-denied|stop-stuck)$/);
  if (stopped && req.method === 'POST') {
    const key = `kubernetes-debug:${stopped[1]}`;
    const pod = await pm.state.get(key);
    const body = await incoming;
    const container = pod?.status?.ephemeralContainerStatuses?.find(item => item.name === body.container);
    if (!container) return send(res, 422, status('Invalid', 'No debug container', 422));
    if (stopped[1] !== 'stop-stuck') {
      container.state = {terminated:{exitCode:0,reason:'Completed'}};
      await pm.state.set(key, pod);
    }
    return send(res, 200, {status:'Success'});
  }
  const match = path.match(/^\/api\/v1\/namespaces\/default\/pods\/(ready|runtime-error|stop-ready|stop-denied|stop-stuck)(\/ephemeralcontainers)?$/);
  if (!match) return send(res, 501, status('NotImplemented', 'Outside this debug fixture contract', 501));
  const name = match[1];
  const key = `kubernetes-debug:${name}`;
  let pod = await pm.state.get(key);
  if (!pod) pod = {apiVersion:'v1',kind:'Pod',metadata:{name,namespace:'default',uid:`uid-${name}`,resourceVersion:'1'},spec:{containers:[{name:'app',image:'fixture'}]},status:{phase:'Running'}};
  if (req.method === 'PATCH' && match[2]) {
    const body = await incoming;
    const patch = typeof body === 'string' ? JSON.parse(body) : body;
    if (patch?.metadata?.uid !== pod.metadata.uid || patch?.metadata?.resourceVersion !== pod.metadata.resourceVersion)
      return send(res, 409, status('Conflict', 'Pod identity or version changed', 409));
    const containers = patch?.spec?.ephemeralContainers;
    if (!Array.isArray(containers) || containers.length !== 1)
      return send(res, 422, status('Invalid', 'Expected one ephemeral container', 422));
    const container = containers[0];
    pod.spec.ephemeralContainers = [...(pod.spec.ephemeralContainers || []), container];
    pod.metadata.resourceVersion = String(Number(pod.metadata.resourceVersion) + 1);
    pod.status.ephemeralContainerStatuses = [{name:container.name, state: name === 'runtime-error'
      ? {waiting:{reason:'CreateContainerError', message:'runtime could not join the target container namespaces'}}
      : {running:{startedAt:'2026-10-21T00:00:00Z'}}}];
    await pm.state.set(key, pod);
    return send(res, 200, pod);
  }
  if (req.method === 'GET' && !match[2]) return send(res, 200, pod);
  return send(res, 501, status('NotImplemented', 'Outside this debug fixture contract', 501));
});

server.listen(process.env.PORT || 4917);
