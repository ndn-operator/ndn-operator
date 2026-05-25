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

Create router credentials, a separate application trust hierarchy, and the Networks:

```shell
kubectl --context rancher-desktop apply -f certificates.yaml
kubectl --context rancher-desktop -n mynetwork wait --for=condition=Ready \
  certificate/subnetwork1-root certificate/subnetwork2-root \
  certificate/app-root certificate/app-subnetwork1-ca certificate/app-subnetwork2-ca \
  certificate/app-subnetwork1-helloworld certificate/app-subnetwork2-helloworld \
  --timeout=5m
kubectl --context rancher-desktop apply -f networks.yaml
kubectl --context rancher-desktop -n mynetwork wait --for=condition=Ready \
  network/subnetwork1 network/subnetwork2 --timeout=5m
kubectl --context rancher-desktop -n mynetwork rollout status daemonset/subnetwork1 --timeout=5m
kubectl --context rancher-desktop -n mynetwork rollout status daemonset/subnetwork2 --timeout=5m
kubectl --context rancher-desktop apply -f neighbor.yaml
```

Start signed producers, then run validating consumers in the opposite Network:

```shell
kubectl --context rancher-desktop apply -f producers.yaml
kubectl --context rancher-desktop -n mynetwork wait --for=condition=Ready \
  pod/subnetwork1-helloworld-producer pod/subnetwork2-helloworld-producer \
  pod/subnetwork2-helloworld-forger --timeout=5m
kubectl --context rancher-desktop -n mynetwork delete job \
  subnetwork1-to-subnetwork2-helloworld subnetwork2-to-subnetwork1-helloworld \
  subnetwork1-rejects-subnetwork2-forgery \
  --ignore-not-found
kubectl --context rancher-desktop apply -f consumers.yaml
kubectl --context rancher-desktop -n mynetwork wait --for=condition=Complete \
  job/subnetwork1-to-subnetwork2-helloworld job/subnetwork2-to-subnetwork1-helloworld \
  job/subnetwork1-rejects-subnetwork2-forgery --timeout=3m
kubectl --context rancher-desktop -n mynetwork logs job/subnetwork1-to-subnetwork2-helloworld
kubectl --context rancher-desktop -n mynetwork logs job/subnetwork2-to-subnetwork1-helloworld
kubectl --context rancher-desktop -n mynetwork logs job/subnetwork1-rejects-subnetwork2-forgery
```

The first two Jobs log `VERIFIED data` for legitimate signed content. The
third logs `REJECTED data` because the `subnetwork2` identity is not
authorized to answer below the protected `subnetwork1` prefix.
