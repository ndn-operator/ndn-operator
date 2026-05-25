# TODO

- Replace deprecated `serde_yaml` with a maintained YAML serialization path for CRD generation and pod init config output.
- Add standard container-level resource customization to the Network router DaemonSet template.
- Document the GKE integration workflow after the test automation settles.

## Certificate-aware subnetwork application example

Add a small certificate-aware `helloworld` producer/consumer example for
`examples/subnetworks/local`. The existing `ndnd pingserver` and `ndnd ping`
workloads prove routing only: `pingserver` always uses a SHA-256 digest signer,
and `ping` does not validate returned Data. They cannot demonstrate that
`/root-network/subnetwork2` is unable to impersonate application Data under
`/root-network/subnetwork1`.

### Security contract

- Keep `Network.spec.dvNetwork: /root-network` for the shared routing domain.
- Treat the existing `subnetwork1-root` and `subnetwork2-root` certificates as
  router/DV credentials, not application namespace authorization.
- Add a separately trusted application certificate hierarchy, for example:
  `/root-network` application root, delegated
  `/root-network/subnetwork1` and `/root-network/subnetwork2` application CAs,
  and leaf producer certificates below each subnetwork prefix.
- Pin the application root certificate in validating clients and use an LVS
  trust schema that binds Data under each subnetwork prefix to its delegated
  application CA. Add CrossSchema authorization only for explicit cross-prefix
  delegation cases.
- Document the remaining routing limitation: a peer in the shared DV domain
  can still advertise a sibling prefix and blackhole Interests, even though it
  cannot return Data accepted by a correctly configured validating client.

### Implementation plan

- Add application `Certificate` resources and example workloads for a simple
  signed `helloworld` producer and validating consumer.
- Update the injector API and webhook mutation behavior so an attached
  application can request application credentials. A signing producer must
  receive its private key and certificate chain; a validating consumer must
  receive trusted application root certificate material and its trust schema.
  Keep router certificate injection/configuration separate from application
  identity material.
- Decide and document the public workload contract for application security,
  including annotations or Network fields, Secret mounts, container paths, and
  any environment variables; add webhook tests before implementation.
- Build the example application on the `ndnd/std` trust APIs rather than the
  `ndnd pingserver`/`ping` CLI tools.
- Add an acceptance test where a legitimate
  `/root-network/subnetwork1/helloworld` producer validates successfully, while
  a subnetwork 2 producer claiming that same name is rejected. Include an
  optional explicit CrossSchema delegation case after the negative test works.

### References

- [NDN trust model](https://101.named-data.net/security/trust-model/)
- [Light VerSec trust schema documentation](https://python-ndn.readthedocs.io/en/latest/src/lvs/lvs.html)
- [`ndnd` CrossSchema documentation](https://github.com/named-data/ndnd/blob/v1.5.2/docs/cross-schema.md)
- [`ndnd` trust configuration implementation](https://github.com/named-data/ndnd/blob/v1.5.2/std/security/trust_config.go)
- [`ndnd pingserver` digest signer implementation](https://github.com/named-data/ndnd/blob/v1.5.2/tools/pingserver.go)
- [`ndnd ping` client implementation](https://github.com/named-data/ndnd/blob/v1.5.2/tools/pingclient.go)
- [`ndnd` DV protocol specification](https://github.com/named-data/ndnd/blob/v1.5.2/dv/SPEC.md)
