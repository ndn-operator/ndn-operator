# GKE integration test infrastructure

This OpenTofu module creates two ephemeral private-node GKE clusters for PR
integration tests. The primary cluster is dual-stack in `us-west1-a`; an
isolated IPv4 peer cluster defaults to `us-south1-a` in its own VPC. Both
clusters keep the control plane public, but limit access with master authorized
networks to the current GitHub runner IP.

The data-path test exposes a WebSocket face through a public LoadBalancer in the
primary cluster. The peer cluster reaches that endpoint over its own Cloud NAT,
so there is no VPC peering or private cross-region connectivity. The peer
enables WebSocket behind an internal `ClusterIP` service only, allowing its
outbound `Neighbor` link without publishing a second public endpoint.

The workflow runs when a trusted pull request has the `run-gke-integration`
label.

The GitHub workflow uses direct Workload Identity Federation.

No service account impersonation is configured in the workflow. Grant IAM roles
directly to the workload identity principal that represents this repository.

Required repository variables:

- `GKE_INTEGRATION_PROJECT_ID`: Google Cloud project for ephemeral resources,
  currently `ndn-operator-test`.
- `GKE_INTEGRATION_STATE_BUCKET`: GCS bucket for OpenTofu state, currently
  `ndn-operator-integration-test`.

Optional repository variables:

- `GKE_INTEGRATION_REGION`: defaults to `us-west1`.
- `GKE_INTEGRATION_LOCATION`: defaults to `us-west1-a`.
- `GKE_INTEGRATION_MACHINE_TYPE`: defaults to `n4a-standard-2`.
- `GKE_INTEGRATION_PEER_REGION`: defaults to `us-south1`.
- `GKE_INTEGRATION_PEER_LOCATION`: defaults to `us-south1-a`.
- `GKE_INTEGRATION_PEER_MACHINE_TYPE`: defaults to `e2-standard-2`.

The direct WIF principal needs enough access to create and delete the test
cluster and its network. A practical starting point is:

- `roles/compute.networkAdmin` on the project.
- `roles/container.admin` on the project.
- `roles/storage.objectAdmin` and `roles/storage.legacyBucketReader` on the
  state bucket.
- `roles/iam.serviceAccountUser` on the GKE node service account, usually the
  Compute Engine default service account unless the module is changed to use a
  dedicated node service account.

The project should already have the Compute Engine and Kubernetes Engine APIs
enabled, and the state bucket should already exist.

The test installs the PR image tag produced by the existing Docker workflow:
`ghcr.io/<owner>/<repo>:pr-<number>`.

The workflow is intentionally label-gated because it deploys PR-built code into a
credentialed cloud environment.
