# WebSocket multi-cluster example

This example connects two independently secured NDN networks through a public
WebSocket face exposed by cluster 1. Cluster 2 enables WebSocket behind an
internal `ClusterIP` service only, because `ndnd` needs that transport enabled
to initiate its outbound Neighbor link. Each cluster creates its own local root
certificate; only the public root certificate is imported into the other
cluster. Run the following commands from this directory after installing the
operator in both clusters.

Create the namespace and local roots in both clusters:

```shell
kubectl --context cluster-1 create namespace mynetwork
kubectl --context cluster-2 create namespace mynetwork
kubectl --context cluster-1 apply -f cluster-1/certificate-1.yaml
kubectl --context cluster-2 apply -f cluster-2/certificate-2.yaml
kubectl --context cluster-1 -n mynetwork wait --for=condition=Ready certificate/test1-root --timeout=5m
kubectl --context cluster-2 -n mynetwork wait --for=condition=Ready certificate/test2-root --timeout=5m
```

Export each public root and import it into the other cluster:

```shell
secret1="$(kubectl --context cluster-1 -n mynetwork get certificate test1-root -o jsonpath='{.status.cert.secret}')"
secret2="$(kubectl --context cluster-2 -n mynetwork get certificate test2-root -o jsonpath='{.status.cert.secret}')"
kubectl --context cluster-1 -n mynetwork get secret "${secret1}" -o jsonpath='{.data.ndn\.cert}' | base64 --decode > /tmp/test1-root.cert
kubectl --context cluster-2 -n mynetwork get secret "${secret2}" -o jsonpath='{.data.ndn\.cert}' | base64 --decode > /tmp/test2-root.cert
kubectl --context cluster-2 -n mynetwork create secret generic test1-root-imported --from-file=ndn.cert=/tmp/test1-root.cert
kubectl --context cluster-1 -n mynetwork create secret generic test2-root-imported --from-file=ndn.cert=/tmp/test2-root.cert
kubectl --context cluster-1 apply -f cluster-1/external-certificate-2.yaml
kubectl --context cluster-2 apply -f cluster-2/external-certificate-1.yaml
kubectl --context cluster-1 -n mynetwork wait --for=condition=Ready externalcertificate/test2-root --timeout=5m
kubectl --context cluster-2 -n mynetwork wait --for=condition=Ready externalcertificate/test1-root --timeout=5m
kubectl --context cluster-1 apply -f cluster-1/network-1.yaml
kubectl --context cluster-2 apply -f cluster-2/network-2.yaml
```

When the WebSocket LoadBalancer has an external IPv4 address, substitute it
into the cluster 2 Neighbor manifest and apply it:

```shell
address="$(kubectl --context cluster-1 -n mynetwork get service test1-ws -o jsonpath='{.status.loadBalancer.ingress[0].ip}')"
sed "s/<address>/${address}/" cluster-2/neighbor-2.yaml | kubectl --context cluster-2 apply -f -
```

Cluster 2 can now learn routes exposed by cluster 1 over the WebSocket link.
Only cluster 1 publishes a public WebSocket endpoint; cluster 2's `test2-ws`
service remains internal to its cluster.
