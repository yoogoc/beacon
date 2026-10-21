# Debug-container simulation

This local fixture covers `GET Pod` and `PATCH Pod/ephemeralcontainers` from the
[Kubernetes Pod API](https://kubernetes.io/docs/reference/kubernetes-api/workload-resources/pod-v1/).
It checks UID/resourceVersion guards, preserves existing ephemeral containers,
and exposes successful startup and `CreateContainerError` scenarios. Other calls
return 501. The published-mock lookup was unavailable without a Postman login;
the fixture uses the official contract and this repository's typed Kubernetes client.

Start the simulation with Postman CLI 1.70 or later:

```sh
postman simulation run postman/simulations/debug-container.sim.yaml --port 4917 --no-history --output ndjson
```

Then run the real Rust client against it:

```sh
BEACON_DEBUG_SIMULATION_URL=http://127.0.0.1:4917/kubernetes mbx test -p beacon-kube postman_simulation_exercises_debug_creation_and_runtime_failure -- --ignored --test-threads=1
```

Restart the simulation before rerunning the test to reset its state. All Pod
data is synthetic, no kubeconfig is read, and no real cluster is contacted.
These checks cover HTTP creation/status handling; terminal keyboard dispatch,
ANSI rendering, and interactive shell setup have separate Rust/UI tests.

## Managed debug-container stop contract

New Beacon debug containers use a versioned, cooperative keepalive command. A
stop first reads the Pod and verifies its UID and the named ephemeral container's
exact managed command. It then uses `pods/exec` in that container to write its
private stop request and polls the Pod until that container is `terminated`.
Closing a terminal only disconnects it. No Pod deletion, PID signalling, or
business-container restart is part of stopping a debug container.

Already terminated containers are an idempotent success. Replaced Pods, normal
containers, unsupported legacy commands, unavailable container state, exec/RBAC
errors and status timeouts must not be reported as a successful stop. Old debug
containers remain visible but do not gain a stop capability retroactively.

Postman CLI 1.75's HTTP simulation does not forward WebSocket upgrades. To test
the real kube-rs exec framing, run the local wire adapter in a second terminal:

```sh
BEACON_DEBUG_HTTP_URL=http://127.0.0.1:4917/kubernetes node postman/exec-proxy.js
```

The adapter sends all REST requests to the Postman simulation, handles the exec
WebSocket frames, and advances its synthetic container status through a
fixture-only route. With a fresh simulation, run:

```sh
BEACON_DEBUG_SIMULATION_URL=http://127.0.0.1:4918 mbx test -p beacon-kube postman_simulation_stops_only -- --ignored --test-threads=1
```

Inspect both the simulation request log and the adapter's exec log. Actual
keepalive/stop shell scripts also run in isolated temporary directories in the
Rust tests. This establishes client, wire and script behavior locally; it does
not establish a live container runtime's status-reporting latency.
