# AGENTS.md

Guidance for coding agents working in this repository.

## Project Snapshot

`ndn-operator` is a Rust 2024 Kubernetes operator that runs Named Data
Networking inside a cluster. It manages `ndnd` router pods with DaemonSets,
creates and updates NDN-related CRDs, and provides a mutating admission webhook
that injects an `ndnd` Unix socket into labeled workloads.

The public Kubernetes-facing contract matters. Be careful with CRD schemas,
status fields, labels, annotations, Helm template paths, webhook paths, and
container environment variables. They are consumed by the chart, example
manifests, and running clusters.

Useful NDN references:

- NDN architecture overview: https://named-data.net/project/archoverview/
- NDN packet specification: https://docs.named-data.net/NDN-packet-spec/current/
- `ndnd` implementation used by the operator image: https://github.com/named-data/ndnd

## Domain Notes

NDN routes by hierarchical data names rather than endpoint addresses. Consumers
send Interests for names; producers or caches answer with Data packets. In this
repo, a Network CRD describes an NDN prefix and daemon configuration, Routers
represent `ndnd` instances, faces/links are the transports between daemons, and
certificates/trust anchors secure NDN identities.

The operator currently shells out to `/ndnd` for key generation, certificate
signing, and DV link management. Keep those command contracts stable unless the
image and all callers are changed together.

## Repository Map

- `src/main.rs`: `ndnctl`, the main controller binary and diagnostics HTTP
  server.
- `src/lib.rs`: shared error/result types, module exports, telemetry.
- `src/macros/controller.rs`: `controller_scaffold!`, used by controller entry
  points for consistent runtime wiring.
- `src/network_controller/`: `Network` CRD, DaemonSet/RBAC/Service creation,
  router certificate setup, network status.
- `src/router_controller/`: `Router` CRD and router status/neighbor propagation.
- `src/neighbor_controller/`: `Neighbor` CRD and outer neighbor propagation.
- `src/pod_controller/`: watches managed pods and creates per-pod Routers and
  optional Certificates.
- `src/cert_controller/`: `Certificate` CRD, key/cert generation, renewal, and
  Kubernetes Secret management.
- `src/ext_cert_controller/`: `ExternalCertificate` CRD backed by a user Secret.
- `src/bin/injector.rs`: HTTPS mutating admission webhook for workload socket
  injection.
- `src/bin/pod_init.rs`: init container that writes `ndnd` config and key files,
  then patches Router initialization status.
- `src/bin/pod_sidecar.rs`: sidecar that syncs Router neighbors into `ndnd` DV
  links.
- `src/bin/gencrd.rs`: CRD YAML generator for Helm templates.
- `charts/ndn-operator/`: Helm chart, checked-in CRDs, injector webhook config,
  controller deployments.
- `examples/`: sample Networks, Certificates, ExternalCertificates, and
  workloads.
- `Dockerfile`: multi-binary image build; final image includes `/ndnd` from the
  upstream `ndnd` image and the Rust operator binaries.
- `.github/workflows/`: CI build, test, lint, CRD generation, Docker publish,
  and cargo-deny audit workflows.
- `deny.toml`: cargo-deny policy for advisories, duplicate crates, sources, and
  licenses.
- `TODO.md`: short project-level follow-up list; check it before widening a
  change.

## Core Workflows

The main controller binary runs all controllers by default. CLI flags narrow the
set: `--nw`, `--rt`, `--neighbor`/`--nl`, `--pod`, `--cert`, and `--ext-cert`.
It also serves JSON diagnostics on `/` and health on `/health`.

The injector is a separate binary. It must continue serving HTTPS on the
configured IP/port, with `POST /` accepting and returning Kubernetes
`AdmissionReview` JSON. The Helm chart currently targets this exact path.

The DaemonSet-managed `ndnd` pods include:

- an init container (`/init`) that writes config/cert material and patches Router
  status,
- the `ndnd` process itself,
- a sidecar (`/sidecar`) that keeps DV links in sync.

## Controller Patterns

Use `controller_scaffold!` for new controllers or substantial controller entry
point changes. The macro defines the shared `Context`, `State`, `Diagnostics`,
error policy, `run_*` function, shutdown handling, recorder, and preflight
support.

Reconcile functions generally accept `Arc<Resource>` and `Arc<Context>` and
return `Result<Action>`. Prefer server-side apply for owned Kubernetes objects
when matching existing controller style. Emit Kubernetes events through
`events_helper::emit_info`, and ignore event failures unless the event itself is
the feature being changed.

Status updates should be narrow and intentional. The `Conditions` helper
preserves `last_transition_time` when the condition status does not change, and
uses `k8s_openapi::jiff::Timestamp` through `k8s_openapi::apimachinery` types.

Finalizers are used where cleanup affects other resources. Do not remove them
without replacing the cleanup path.

## Kubernetes Contracts

Stable labels and annotations include:

- `network.named-data.net/name`
- `network.named-data.net/namespace`
- `network.named-data.net/managed-by`
- `named-data.net/inject`
- pod annotations `networks.named-data.net/name` and
  `networks.named-data.net/namespace`

Stable injector environment variables include:

- `NDN_INJECTOR_IP`, default `0.0.0.0`
- `NDN_INJECTOR_PORT`, default `8443`
- `NDN_INJECTOR_TLS_CERT_FILE`, default `tls.crt`
- `NDN_INJECTOR_TLS_KEY_FILE`, default `tls.key`

Stable pod/runtime environment variables include:

- `NDN_NETWORK_NAME`
- `NDN_NETWORK_NAMESPACE`
- `NDN_ROUTER_NAME`
- `NDN_NODE_NAME`
- `NDN_SOCKET_PATH`
- `NDN_KEYS_DIR`
- `NDN_INSECURE`
- `NDN_IP_FAMILY`
- `NDN_TRUST_ANCHORS`
- `NDN_CLIENT_TRANSPORT`
- `NDN_PREFIX`

When a CRD schema changes, regenerate the checked-in CRDs under
`charts/ndn-operator/templates/crd` and inspect the diff carefully. CRD changes
can affect Helm installs and upgrades.

## Dependency Notes

`actix-web` is used for both the diagnostics server and the injector webhook.
The injector's TLS path uses `rustls` 0.23 plus `rustls-pki-types`; do not
reintroduce `warp` or `rustls-pemfile` without checking `cargo-deny` first.

`serde_yaml` is deprecated and still used for CRD/config YAML generation. There
is a TODO entry tracking replacement; avoid expanding its use.

`k8s-openapi` 0.27 uses `jiff` timestamps in Kubernetes `Time`. Avoid assuming
`chrono::DateTime<Utc>` fits directly into Kubernetes API types.

`deny.toml` intentionally skips a small set of duplicate transitive crates.
Update those skips only after inspecting `cargo tree -d` and keep the list as
small as possible.

## Verification

Use the narrowest command that proves the change, then broaden when contracts or
shared code are touched.

Common commands:

```shell
cargo fmt --all -- --check --style-edition 2024
cargo check
cargo test
cargo test --workspace
cargo clippy --all-targets --all-features
cargo deny check bans
cargo deny check advisories
cargo run --package ndn-operator --bin gencrd -- --output ./charts/ndn-operator/templates/crd
git diff --exit-code ./charts/ndn-operator/templates/crd
```

CI also runs:

```shell
cargo build --all --locked
cargo test --all --locked
```

For binary-specific dependency or framework changes, run the matching check, for
example `cargo check --bin injector`.

## Change Safety Notes

- Keep behavior-preserving refactors separate from API/schema changes.
- Keep Helm chart changes synchronized with Rust runtime defaults.
- Preserve admission behavior for malformed objects by returning an
  AdmissionReview denial where the existing code does so.
- Do not change object ownership, finalizers, manager names, or labels casually;
  they drive cleanup and cross-controller discovery.
- Prefer existing helper APIs and local patterns over new abstractions.
- For generated CRDs, run the generator and review the YAML diff instead of
  hand-editing generated files.
