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
