# NDN Operator

**NDN Operator** is a Kubernetes operator that integrates [Named Data Networking](https://github.com/named-data) into your Kubernetes cluster

## Install
```
helm repo add ndn-operator https://ndn-operator.github.io/ndn-operator
helm install ndn-operator-crd ndn-operator/ndn-operator-crd
helm install ndn-operator ndn-operator/ndn-operator --namespace ndn-operator --create-namespace
```
## Create your first ndn network
network.yaml:
```yaml
apiVersion: named-data.net/v1alpha1
kind: Network
metadata:
  name: test
spec:
  prefix: /test
  udpUnicastPort: 6363
```
```
kubectl apply -f network.yaml
```
