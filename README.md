# NDN Operator

**NDN Operator** is a Kubernetes operator that integrates [Named Data Networking](https://github.com/named-data) into your Kubernetes cluster

## Architecture
```mermaid
flowchart TB
  subgraph N[Network]
  direction LR
    subgraph n_A[Node A]
    direction TB
      app_a1[Container 1]:::dotted <--unix socket--> ndnd_a[NDN daemon]
    end
    subgraph n_B[Node B]
    direction TB
      app_b2[Container 2]:::dotted <--unix socket--> ndnd_b[NDN daemon]
    end
    subgraph n_C[Node C]
    direction TB
      app_b3[Container 3]:::dotted <--unix socket--> ndnd_c[NDN daemon]
    end
    n_A <--ip--> n_B <--ip--> n_C
  end
  subgraph operator[NDN Operator]
  direction LR
  controller --watch--> C@{ shape: docs, label: "K8S Resources"}
  end
  operator --manage--> N
  classDef dotted stroke-dasharray: 5 5
```

## Features
* Multiple networks per cluster
* TLS management for ndnd
* Multi-cluster support

## Roadmap
1. Basic functionality
    * `Network` resource that creates a simple unsecured network
    * Pod annotations, assigning it to a particular network
1. TLS
    * Self-signed root CA
1. Advanced use
    * Expose NDN faces outside
    * Obtain certificates from Testbed
    * K8S resources to manage NDN faces, strategies and links