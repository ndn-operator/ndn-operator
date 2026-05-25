# Local subnetwork routing example

This example runs two secured application namespaces on one Kubernetes node:

- `/root-network/subnetwork1`
- `/root-network/subnetwork2`

Each `Network` keeps its child prefix for injected applications, while
`spec.dvNetwork: /root-network` places both sets of `ndnd` routers in the same
distance-vector routing domain. The two router certificate authorities are
therefore also rooted at `/root-network`.

`subnetwork1` listens for TCP neighbors through a `ClusterIP` Service, and
`subnetwork2` creates one outbound link to it. That single link is sufficient
for bidirectional DV and prefix synchronization. The two Networks use UDP
ports `6363` and `6364`, allowing their host-network DaemonSet pods to run on
the same Rancher Desktop node.

The `dvNetwork` field is part of the operator source containing this example.
An older installed CRD or operator image cannot run these manifests correctly;
install the chart and image built from this checkout before applying them.

## Run on Rancher Desktop

Build the operator image for the local arm64 Rancher Desktop node:

```shell
docker build --platform linux/arm64 \
  --build-arg TARGET=aarch64-unknown-linux-musl \
  -t ndn-operator:subnetworks-local ../../..
```

Install the locally built operator and its current CRDs:

```shell
helm --kube-context rancher-desktop upgrade --install ndn-operator ../../../charts/ndn-operator \
  --namespace ndn-operator --create-namespace \
  --set image.repository=ndn-operator \
  --set image.tag=subnetworks-local \
  --set image.pullPolicy=IfNotPresent \
  --wait --timeout 5m
kubectl --context rancher-desktop create namespace mynetwork --dry-run=client -o yaml \
  | kubectl --context rancher-desktop apply -f -
```

Create the routing roots and Networks:

```shell
kubectl --context rancher-desktop apply -f certificates.yaml
kubectl --context rancher-desktop -n mynetwork wait --for=condition=Ready \
  certificate/subnetwork1-root certificate/subnetwork2-root --timeout=5m
kubectl --context rancher-desktop apply -f networks.yaml
kubectl --context rancher-desktop -n mynetwork wait --for=condition=Ready \
  network/subnetwork1 network/subnetwork2 --timeout=5m
kubectl --context rancher-desktop -n mynetwork rollout status daemonset/subnetwork1 --timeout=5m
kubectl --context rancher-desktop -n mynetwork rollout status daemonset/subnetwork2 --timeout=5m
kubectl --context rancher-desktop apply -f neighbor.yaml
```

Start both producers and run consumers in the opposite Network:

```shell
kubectl --context rancher-desktop apply -f pingservers.yaml
kubectl --context rancher-desktop -n mynetwork wait --for=condition=Ready \
  pod/subnetwork1-pingserver pod/subnetwork2-pingserver --timeout=5m
kubectl --context rancher-desktop -n mynetwork delete job \
  subnetwork1-to-subnetwork2-ping subnetwork2-to-subnetwork1-ping \
  --ignore-not-found
kubectl --context rancher-desktop apply -f ping-jobs.yaml
kubectl --context rancher-desktop -n mynetwork wait --for=condition=Complete \
  job/subnetwork1-to-subnetwork2-ping job/subnetwork2-to-subnetwork1-ping --timeout=3m
kubectl --context rancher-desktop -n mynetwork logs job/subnetwork1-to-subnetwork2-ping
kubectl --context rancher-desktop -n mynetwork logs job/subnetwork2-to-subnetwork1-ping
```

Each Job log should contain `content from` lines for the other child prefix.
