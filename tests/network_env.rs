use operator::network_controller::{IpFamily, NdndSpec, Network, NetworkSpec};

#[test]
fn default_ndnd_image() {
    let spec = NetworkSpec {
        prefix: "/test".into(),
        dv_network: None,
        udp_unicast_port: 6363,
        ip_family: IpFamily::IPv4,
        node_selector: None,
        template: None,
        ndnd: NdndSpec::default(),
        operator: None,
        router_cert_issuer: None,
        trust_anchors: None,
        faces: None,
    };
    // Just ensure defaults compile & basic field passes through
    assert_eq!(spec.ndnd.image, "ghcr.io/named-data/ndnd:latest");
}

fn network_with_prefix_and_dv_network(dv_network: Option<&str>) -> Network {
    Network::new(
        "subnetwork1",
        NetworkSpec {
            prefix: "/root-network/subnetwork1".into(),
            dv_network: dv_network.map(str::to_string),
            udp_unicast_port: 6363,
            ip_family: IpFamily::IPv4,
            node_selector: None,
            template: None,
            ndnd: NdndSpec::default(),
            operator: None,
            router_cert_issuer: None,
            trust_anchors: None,
            faces: None,
        },
    )
}

#[test]
fn explicit_dv_network_defines_the_routing_domain() {
    let network = network_with_prefix_and_dv_network(Some("/root-network"));
    assert_eq!(network.dv_network(), "/root-network");
}

#[test]
fn application_prefix_is_the_default_routing_domain() {
    let network = network_with_prefix_and_dv_network(None);
    assert_eq!(network.dv_network(), "/root-network/subnetwork1");
}
