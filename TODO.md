# TODO

- Replace deprecated `serde_yaml` with a maintained YAML serialization path for CRD generation and pod init config output.
- Add standard container-level resource customization to the Network router DaemonSet template.
- Document the GKE integration workflow after the test automation settles.

## Certificate-aware subnetwork application example

The signed `helloworld` example replaces routing-only `ndnd ping` assertions.
Its Rust producer and consumer live in `ndn-helloworld-rs`, using
`Quarmire/ndn-rs` pinned to
`991ab06233d0b7596eda30e70fe069659be50042`. The application includes its LVS
policy asset and the adapter for operator-generated NDN PEM/SEC1 credentials.

Application authentication is separate from router/DV authentication:

- Router roots still establish the shared `/root-network` DV domain.
- Application roots, delegated subnetwork CAs, and leaf producer identities
  authorize Data beneath `/root-network/<subnetwork>/helloworld`.
- The injector mounts signing keys only in the container named by
  `credentials.named-data.net/container`, and mounts anchors separately from
  untrusted chain material.
- Local and GKE fixtures validate legitimate signed Data and deliberately
  reject a `subnetwork2` leaf serving a `subnetwork1` Data name.

The security check proves authenticated Data acceptance, not exclusive routing
ownership: a peer in the shared DV domain can still advertise a sibling prefix
and disrupt reachability even when it cannot produce acceptable signed Data.

The integration manifests use the published application image
`ghcr.io/ndn-operator/ndn-helloworld-rs:0.1.0`.

### References

- [NDN trust model](https://101.named-data.net/security/trust-model/)
- [Light VerSec trust schema documentation](https://python-ndn.readthedocs.io/en/latest/src/lvs/lvs.html)
- [`ndnd` CrossSchema documentation](https://github.com/named-data/ndnd/blob/v1.5.2/docs/cross-schema.md)
- [`ndnd` trust configuration implementation](https://github.com/named-data/ndnd/blob/v1.5.2/std/security/trust_config.go)
- [`ndnd` DV protocol specification](https://github.com/named-data/ndnd/blob/v1.5.2/dv/SPEC.md)
