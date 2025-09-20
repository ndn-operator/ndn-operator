use operator::network_controller::{NdndSpec, NetworkSpec};

#[test]
fn default_ndnd_image() {
    let spec = NetworkSpec {
        prefix: "/test".into(),
        udp_unicast_port: 6363,
        node_selector: None,
        ndnd: NdndSpec::default(),
        operator: None,
        router_cert_issuer: None,
        trust_anchors: None,
        faces: None,
    };
    // Just ensure defaults compile & basic field passes through
    assert_eq!(spec.ndnd.image, "ghcr.io/named-data/ndnd:latest");
}
