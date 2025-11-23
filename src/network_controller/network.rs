use super::Context;
use crate::conditions::Conditions;
use crate::{
    Error, Result,
    cert_controller::{
        Certificate, CertificateSpec, ExternalCertificate, IssuerRef, is_cert_valid,
        is_external_cert_valid,
    },
    events_helper::emit_info,
    helper::get_my_image,
    router_controller::{CertificateRef, Router, RouterSpec},
};
use duration_string::DurationString;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition as K8sCondition;
use k8s_openapi::{
    api::{
        apps::v1::DaemonSet,
        core::v1::{Service, ServiceAccount},
        rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject},
    },
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{
    Client, CustomResource, Resource,
    api::{Api, Patch, PatchParams, ResourceExt},
    runtime::{
        controller::Action,
        events::{Event, EventType},
        wait::await_condition,
    },
};
use operator_derive::Conditions;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_with::skip_serializing_none;
use std::{collections::BTreeMap, sync::Arc, time::Duration as StdDuration};
use tracing::*;

pub static NETWORK_FINALIZER: &str = "network.named-data.net/finalizer";
pub static NETWORK_MANAGER_NAME: &str = "network-controller";
pub static NETWORK_LABEL_KEY: &str = "network.named-data.net/name";
pub static DS_LABEL_KEY: &str = "network.named-data.net/managed-by";
pub static CONTAINER_CONFIG_DIR: &str = "/etc/ndnd";
pub static CONTAINER_SOCKET_DIR: &str = "/run/ndnd";
pub static CONTAINER_KEYS_DIR: &str = "/etc/ndn/keys";
// The host directories where the configuration and socket files will be stored
// Subdirectories are created for each namespace
pub static HOST_CONFIG_ROOT_DIR: &str = "/etc/ndnd";
pub static HOST_SOCKET_ROOT_DIR: &str = "/run/ndnd";
pub static HOST_KEYS_ROOT_DIR: &str = "/etc/ndn/keys";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[kube(
    group = "named-data.net",
    version = "v1alpha1",
    kind = "Network",
    derive = "Default",
    namespaced,
    shortname = "nw",
    doc = "Network represents a Named Data Networking (NDN) network in Kubernetes",
    printcolumn = r#"{"name":"Prefix","jsonPath":".spec.prefix","type":"string"}"#,
    printcolumn = r#"{"name":"UDP Unicast Port","jsonPath":".spec.udpUnicastPort","type":"integer"}"#,
    printcolumn = r#"{"name":"Cert Issuer","jsonPath":".spec.routerCertIssuer.name","type":"string"}"#,
    printcolumn = r#"{"name":"DaemonSet Created","jsonPath":".status.dsCreated","type":"boolean"}"#,
    status = "NetworkStatus"
)]
pub struct NetworkSpec {
    /// The prefix for the network, used for routing and naming conventions.
    /// This should be a valid NDN prefix, e.g., "/example/network"
    pub prefix: String,
    /// The UDP unicast port for the nodes.
    /// Must be unique across all networks in the cluster.
    pub udp_unicast_port: u16,
    /// Preferred IP family for inter-router UDP faces. Defaults to IPv4.
    /// When set to IPv4, only an IPv4 UDP face will be published per router (with IPv6 as a fallback if IPv4 is not available on the node).
    /// When set to IPv6, only an IPv6 UDP face will be published per router (with IPv4 as a fallback if IPv6 is not available on the node).
    #[serde(default = "default_ip_family")]
    #[schemars(default = "default_ip_family")]
    pub ip_family: IpFamily,
    /// The node selector for the network, used to schedule the network controller on specific nodes
    pub node_selector: Option<BTreeMap<String, String>>,
    /// The NDND image to use for the network controller
    #[serde(default = "NdndSpec::default")]
    pub ndnd: NdndSpec,
    /// The operator image to use for the network controller.
    /// If not specified, the operator will use its own image
    pub operator: Option<OperatorSpec>,
    /// The certificate issuer for the router certificates.
    /// If not specified, the network will be insecure (no certificates)
    pub router_cert_issuer: Option<IssuerRef>,
    /// The trust anchors for the network, used for validating certificates
    pub trust_anchors: Option<Vec<TrustAnchorRef>>,
    /// Public faces to expose from router pods
    pub faces: Option<FacesSpec>,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrustAnchorRef {
    /// The name of the trust anchor, e.g., "router-cert"
    pub name: String,
    /// The kind of the trust anchor, e.g., "Certificate".
    /// "Certificate" and "ExternalCertificate" are supported
    pub kind: String,
    /// The namespace of the trust anchor.
    /// If not specified, the network's namespace will be used
    pub namespace: Option<String>,
}

impl TrustAnchorRef {
    pub async fn get_cert_name(&self, client: &Client, default_ns: &str) -> Result<String> {
        match self.kind.as_str() {
            "Certificate" => {
                let api_cert = Api::<Certificate>::namespaced(
                    client.clone(),
                    &self.namespace.clone().unwrap_or(default_ns.to_string()),
                );
                info!("Waiting for the router certificate to be valid...");
                let cert_valid = await_condition(api_cert.clone(), &self.name, is_cert_valid());
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), cert_valid)
                    .await
                    .map_err(|e| {
                        Error::OtherError(format!(
                            "Timeout while waiting for certificate to be valid: {e}"
                        ))
                    })?;
                let cert = api_cert
                    .get_status(&self.name)
                    .await
                    .map_err(Error::KubeError)?;
                cert.status
                    .and_then(|s| s.cert.name)
                    .ok_or(Error::OtherError(
                        "Certificate name not found in status".to_string(),
                    ))
            }
            "ExternalCertificate" => {
                let api_cert = Api::<ExternalCertificate>::namespaced(
                    client.clone(),
                    &self.namespace.clone().unwrap_or(default_ns.to_string()),
                );
                info!("Waiting for the external certificate to be valid...");
                let cert_valid =
                    await_condition(api_cert.clone(), &self.name, is_external_cert_valid());
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), cert_valid)
                    .await
                    .map_err(|e| {
                        Error::OtherError(format!(
                            "Timeout while waiting for external certificate to be valid: {e}"
                        ))
                    })?;
                let cert = api_cert
                    .get_status(&self.name)
                    .await
                    .map_err(Error::KubeError)?;
                cert.status
                    .and_then(|s| s.cert.name)
                    .ok_or(Error::OtherError(
                        "ExternalCertificate name not found in status".to_string(),
                    ))
            }
            _ => Err(Error::OtherError(format!(
                "Unsupported trust anchor kind: {}",
                self.kind
            ))),
        }
    }
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct FaceServiceTemplate {
    /// Standard object metadata for the service
    #[serde(default)]
    #[schemars(default)]
    pub metadata: ObjectMeta,
    /// Specification for the service template
    #[serde(default)]
    #[schemars(default)]
    pub spec: FaceServiceTemplateSpec,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FaceServiceTemplateSpec {
    /// Service type to expose this face (default: LoadBalancer)
    #[serde(default = "default_service_type")]
    #[schemars(default = "default_service_type")]
    pub type_: String,
}

fn default_service_type() -> String {
    "LoadBalancer".to_string()
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkTcpFaceSpec {
    pub port: u16,
    #[serde(default = "default_face_service_template")]
    #[schemars(default = "default_face_service_template")]
    pub service_template: FaceServiceTemplate,
}

impl Default for NetworkTcpFaceSpec {
    fn default() -> Self {
        Self {
            port: 6363,
            service_template: default_face_service_template(),
        }
    }
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkWebSocketFaceSpec {
    pub port: u16,
    #[serde(default = "default_face_service_template")]
    #[schemars(default = "default_face_service_template")]
    pub service_template: FaceServiceTemplate,
}

impl Default for NetworkWebSocketFaceSpec {
    fn default() -> Self {
        Self {
            port: 9696,
            service_template: default_face_service_template(),
        }
    }
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct FacesSpec {
    pub tcp: Option<NetworkTcpFaceSpec>,
    pub websocket: Option<NetworkWebSocketFaceSpec>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub enum IpFamily {
    #[serde(rename = "IPv4")]
    #[default]
    IPv4,
    #[serde(rename = "IPv6")]
    IPv6,
}

fn default_ip_family() -> IpFamily {
    IpFamily::IPv4
}

fn default_face_service_template() -> FaceServiceTemplate {
    FaceServiceTemplate::default()
}

impl Default for FaceServiceTemplateSpec {
    fn default() -> Self {
        Self {
            type_: default_service_type(),
        }
    }
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NdndSpec {
    pub image: String,
}

impl Default for NdndSpec {
    fn default() -> Self {
        Self {
            image: "ghcr.io/named-data/ndnd:latest".to_string(),
        }
    }
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorSpec {
    pub image: String,
}

impl Default for OperatorSpec {
    fn default() -> Self {
        Self {
            image: "ghcr.io/ndn-operator/ndn-operator:latest".to_string(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, Conditions)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStatus {
    /// Indicates whether the DaemonSet for the network has been created
    pub ds_created: Option<bool>,
    /// Standard Kubernetes-style conditions for this network
    /// - Ready: DSCreated && RBACReady
    /// - DSCreated: DaemonSet successfully applied
    /// - RBACReady: ServiceAccount, Role, RoleBinding successfully applied
    pub conditions: Option<Vec<K8sCondition>>,
}

impl Network {
    pub async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        let api_nw: Api<Network> = Api::namespaced(ctx.client.clone(), &self.namespace().unwrap());
        let serverside = PatchParams::apply(NETWORK_MANAGER_NAME);
        let ns = self.namespace().unwrap();
        let api_sa: Api<ServiceAccount> = Api::namespaced(ctx.client.clone(), &ns);
        let api_role: Api<Role> = Api::namespaced(ctx.client.clone(), &ns);
        let api_role_binding: Api<RoleBinding> = Api::namespaced(ctx.client.clone(), &ns);
        let api_ds: Api<DaemonSet> = Api::namespaced(ctx.client.clone(), &ns);
        let sa_data = self.create_owned_sa();
        let role_date = self.create_owned_role();
        let role_binding_data =
            self.create_owned_role_binding(sa_data.name_any(), role_date.name_any());
        let operator_spec = &self.spec.operator;
        let ds_image = match operator_spec {
            Some(op) => op.image.clone(),
            None => get_my_image(ctx.client.clone())
                .await
                .ok()
                .unwrap_or(OperatorSpec::default().image),
        };
        let ds_data = super::daemonset::create_owned_daemonset(
            self,
            Some(ds_image),
            Some(sa_data.name_any()),
        )?;
        // Create ServiceAccount
        let _sa = api_sa
            .patch(&self.name_any(), &serverside, &Patch::Apply(sa_data))
            .await
            .map_err(Error::KubeError)?;
        let _role = api_role
            .patch(&self.name_any(), &serverside, &Patch::Apply(role_date))
            .await
            .map_err(Error::KubeError)?;
        let _role_binding = api_role_binding
            .patch(
                &self.name_any(),
                &serverside,
                &Patch::Apply(role_binding_data),
            )
            .await
            .map_err(Error::KubeError)?;
        // RBAC applied successfully
        let observed_gen = self.metadata.generation.unwrap_or(0);
        let mut s = NetworkStatus {
            ds_created: None,
            conditions: None,
        };
        s.upsert_bool(
            "RBACReady",
            true,
            "Applied",
            Some("ServiceAccount, Role, RoleBinding applied"),
            observed_gen,
        );
        // Create DaemonSet
        let ds = api_ds
            .patch(&self.name_any(), &serverside, &Patch::Apply(ds_data))
            .await
            .map_err(Error::KubeError)?;
        // Publish event
        emit_info(
            &ctx.recorder,
            self,
            "DaemonSetCreated",
            "Created",
            Some(format!(
                "Created `{}` DaemonSet for `{}` Network",
                ds.name_any(),
                self.name_any()
            )),
        )
        .await;
        // Update conditions and status of the Network
        s.upsert_bool(
            "DSCreated",
            true,
            "Created",
            Some("DaemonSet applied"),
            observed_gen,
        );
        // Create/Update Services for configured faces
        if let Some(faces) = &self.spec.faces {
            let api_svc: Api<Service> =
                Api::namespaced(ctx.client.clone(), &self.namespace().unwrap());
            if let Some(tcp) = &faces.tcp {
                let svc_name = format!("{}-tcp", self.name_any());
                let oref = self.controller_owner_ref(&()).unwrap();
                let mut labels = BTreeMap::from([(DS_LABEL_KEY.to_string(), self.name_any())]);
                let selector_labels = labels.clone();
                if let Some(extra_labels) = tcp.service_template.metadata.labels.clone() {
                    labels.extend(extra_labels);
                }
                let svc = Service {
                    metadata: ObjectMeta {
                        name: Some(svc_name.clone()),
                        namespace: self.namespace(),
                        owner_references: Some(vec![oref.clone()]),
                        labels: Some(labels),
                        annotations: tcp.service_template.metadata.annotations.clone(),
                        ..ObjectMeta::default()
                    },
                    spec: Some(k8s_openapi::api::core::v1::ServiceSpec {
                        type_: Some(tcp.service_template.spec.type_.clone()),
                        selector: Some(selector_labels),
                        ports: Some(vec![k8s_openapi::api::core::v1::ServicePort {
                            name: Some("tcp".to_string()),
                            protocol: Some("TCP".to_string()),
                            port: tcp.port as i32,
                            target_port: Some(
                                k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                                    tcp.port as i32,
                                ),
                            ),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let serverside = PatchParams::apply(NETWORK_MANAGER_NAME).force();
                let _ = api_svc
                    .patch(&svc_name, &serverside, &Patch::Apply(&svc))
                    .await
                    .map_err(Error::KubeError)?;
            }
            if let Some(ws) = &faces.websocket {
                let svc_name = format!("{}-ws", self.name_any());
                let oref = self.controller_owner_ref(&()).unwrap();
                let mut labels = BTreeMap::from([(DS_LABEL_KEY.to_string(), self.name_any())]);
                let selector_labels = labels.clone();
                if let Some(extra_labels) = ws.service_template.metadata.labels.clone() {
                    labels.extend(extra_labels);
                }
                let svc = Service {
                    metadata: ObjectMeta {
                        name: Some(svc_name.clone()),
                        namespace: self.namespace(),
                        owner_references: Some(vec![oref.clone()]),
                        labels: Some(labels),
                        ..ObjectMeta::default()
                    },
                    spec: Some(k8s_openapi::api::core::v1::ServiceSpec {
                        type_: Some(ws.service_template.spec.type_.clone()),
                        selector: Some(selector_labels),
                        ports: Some(vec![k8s_openapi::api::core::v1::ServicePort {
                            name: Some("websocket".to_string()),
                            protocol: Some("TCP".to_string()),
                            port: ws.port as i32,
                            target_port: Some(
                                k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                                    ws.port as i32,
                                ),
                            ),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let serverside = PatchParams::apply(NETWORK_MANAGER_NAME).force();
                let _ = api_svc
                    .patch(&svc_name, &serverside, &Patch::Apply(&svc))
                    .await
                    .map_err(Error::KubeError)?;
            }
        }
        // Compute Ready
        let ready = true; // both RBACReady and DSCreated just set true above
        s.upsert_bool(
            "Ready",
            ready,
            if ready {
                "Ready"
            } else {
                "PrerequisitesNotReady"
            },
            None,
            observed_gen,
        );
        let status = json!({
            "status": {
                "dsCreated": true,
                "conditions": s.conditions
            }
        });
        let _o = api_nw
            .patch_status(&self.name_any(), &serverside, &Patch::Merge(&status))
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }

    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        let oref = self.object_ref(&());
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "DeleteRequested".into(),
                    note: Some(format!("Delete `{}`", self.name_any())),
                    action: "Deleting".into(),
                    secondary: None,
                },
                &oref,
            )
            .await
            .map_err(Error::KubeError)?;
        emit_info(
            &ctx.recorder,
            self,
            "DeleteRequested",
            "Deleting",
            Some(format!("Delete `{}`", self.name_any())),
        )
        .await;
        Ok(Action::await_change())
    }

    fn socket_file_name(&self) -> String {
        format!("{}.sock", self.name_any())
    }

    pub fn container_socket_path(&self) -> String {
        format!("{}/{}", CONTAINER_SOCKET_DIR, self.socket_file_name())
    }

    fn config_file_name(&self) -> String {
        format!("{}.yml", self.name_any())
    }

    pub fn container_config_path(&self) -> String {
        format!("{}/{}", CONTAINER_CONFIG_DIR, self.config_file_name())
    }

    pub fn container_keys_dir(&self) -> String {
        CONTAINER_KEYS_DIR.to_string()
    }

    pub fn host_socket_dir(&self) -> String {
        format!(
            "{}/{}/{}",
            HOST_SOCKET_ROOT_DIR,
            self.namespace().unwrap(),
            self.name_any()
        )
    }

    pub fn host_socket_path(&self) -> String {
        format!("{}/{}", self.host_socket_dir(), self.socket_file_name())
    }

    pub fn host_config_dir(&self) -> String {
        format!(
            "{}/{}/{}",
            HOST_CONFIG_ROOT_DIR,
            self.namespace().unwrap(),
            self.name_any()
        )
    }

    pub fn host_config_path(&self) -> String {
        format!("{}/{}", self.host_config_dir(), self.config_file_name())
    }

    pub fn host_keys_dir(&self) -> String {
        format!(
            "{}/{}/{}",
            HOST_KEYS_ROOT_DIR,
            self.namespace().unwrap(),
            self.name_any()
        )
    }

    fn create_owned_sa(&self) -> ServiceAccount {
        let oref = self.controller_owner_ref(&()).unwrap();
        ServiceAccount {
            metadata: ObjectMeta {
                name: Some(self.name_any()),
                owner_references: Some(vec![oref]),
                ..ObjectMeta::default()
            },
            automount_service_account_token: Some(true),
            ..ServiceAccount::default()
        }
    }

    fn create_owned_role(&self) -> Role {
        let oref = self.controller_owner_ref(&()).unwrap();
        Role {
            metadata: ObjectMeta {
                name: Some(self.name_any()),
                owner_references: Some(vec![oref]),
                ..ObjectMeta::default()
            },
            rules: Some(vec![
                PolicyRule {
                    api_groups: Some(vec!["named-data.net".to_string()]),
                    resources: Some(vec!["routers/status".to_string()]),
                    verbs: vec!["get".to_string(), "patch".to_string()],
                    ..PolicyRule::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["named-data.net".to_string()]),
                    resources: Some(vec!["routers".to_string()]),
                    verbs: vec!["get".to_string(), "list".to_string(), "watch".to_string()],
                    ..PolicyRule::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["named-data.net".to_string()]),
                    resources: Some(vec![
                        "certificates".to_string(),
                        "externalcertificates".to_string(),
                    ]),
                    verbs: vec!["list".to_string(), "watch".to_string()],
                    ..PolicyRule::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["named-data.net".to_string()]),
                    resources: Some(vec![
                        "certificates/status".to_string(),
                        "externalcertificates/status".to_string(),
                    ]),
                    verbs: vec!["get".to_string()],
                    ..PolicyRule::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["".to_string()]),
                    resources: Some(vec!["secrets".to_string()]),
                    verbs: vec!["get".to_string()],
                    ..PolicyRule::default()
                },
            ]),
        }
    }

    fn create_owned_role_binding(&self, sa_name: String, role_name: String) -> RoleBinding {
        let oref = self.controller_owner_ref(&()).unwrap();
        RoleBinding {
            metadata: ObjectMeta {
                name: Some(self.name_any()),
                owner_references: Some(vec![oref]),
                ..ObjectMeta::default()
            },
            role_ref: RoleRef {
                api_group: "rbac.authorization.k8s.io".to_string(),
                kind: "Role".to_string(),
                name: role_name,
            },
            subjects: Some(vec![Subject {
                kind: "ServiceAccount".to_string(),
                name: sa_name,
                namespace: Some(self.namespace().unwrap()),
                ..Subject::default()
            }]),
        }
    }

    pub fn create_owned_router(
        &self,
        name: &str,
        node_name: &str,
        cert: Option<Certificate>,
    ) -> Result<Router> {
        let oref = self.controller_owner_ref(&()).ok_or(Error::OtherError(
            "Failed to create owner reference".to_string(),
        ))?;
        let router = Router {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: self.namespace(),
                owner_references: Some(vec![oref]),
                labels: {
                    let mut labels = self.labels().clone();
                    labels.extend(BTreeMap::from([(
                        NETWORK_LABEL_KEY.to_string(),
                        self.name_any(),
                    )]));
                    Some(labels)
                },
                annotations: Some(self.annotations().clone()),
                ..ObjectMeta::default()
            },
            spec: RouterSpec {
                prefix: format!("{}/{}", self.spec.prefix, name),
                node_name: node_name.to_string(),
                cert: cert.map(|c| CertificateRef {
                    name: c.metadata.name.unwrap_or_default(),
                    namespace: c.metadata.namespace,
                }),
            },
            status: None,
        };
        Ok(router)
    }

    pub fn create_owned_certificate(
        &self,
        name: &str,
        router_name: &str,
        cert_issuer: &IssuerRef,
    ) -> Result<Certificate> {
        let oref = self.controller_owner_ref(&()).ok_or(Error::OtherError(
            "Failed to create owner reference for certificate".to_string(),
        ))?;
        let cert = Certificate {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: self.namespace(),
                owner_references: Some(vec![oref]),
                labels: {
                    let mut labels = self.labels().clone();
                    labels.extend(BTreeMap::from([(
                        NETWORK_LABEL_KEY.to_string(),
                        self.name_any(),
                    )]));
                    Some(labels)
                },
                annotations: Some(self.annotations().clone()),
                ..ObjectMeta::default()
            },
            spec: CertificateSpec {
                prefix: format!("{}/{}/32=DV", self.spec.prefix, router_name),
                issuer: cert_issuer.clone(),
                renew_interval: Some(DurationString::new(StdDuration::from_secs(60 * 60 * 24))), // Default to 1 day
                renew_before: Some(DurationString::new(StdDuration::from_secs(60 * 60))), // Default to 1 hour
            },
            ..Certificate::default()
        };
        Ok(cert)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert_controller::IssuerRef;
    use std::collections::BTreeMap;

    fn sample_network() -> Network {
        let spec = NetworkSpec {
            prefix: "/example".into(),
            udp_unicast_port: 6363,
            ip_family: IpFamily::IPv4,
            node_selector: None,
            ndnd: NdndSpec::default(),
            operator: Some(OperatorSpec::default()),
            router_cert_issuer: Some(IssuerRef {
                name: "issuer".into(),
                kind: "Certificate".into(),
                namespace: None,
            }),
            trust_anchors: None,
            faces: None,
        };
        let mut nw = Network::new("demo", spec);
        nw.metadata.namespace = Some("demo-ns".into());
        nw.metadata.uid = Some("uid-123".into());
        nw.metadata.labels = Some(BTreeMap::from([("app".into(), "demo".into())]));
        nw.metadata.annotations = Some(BTreeMap::from([("anno".into(), "value".into())]));
        nw
    }

    #[test]
    fn path_helpers_match_expected_layout() {
        let nw = sample_network();
        assert!(nw.container_socket_path().ends_with("demo.sock"));
        assert!(nw.container_config_path().ends_with("demo.yml"));
        assert!(nw.host_socket_path().contains("demo-ns"));
        assert!(nw.host_config_path().contains("demo-ns"));
        assert!(nw.host_keys_dir().contains("demo-ns"));
    }

    #[test]
    fn create_owned_service_account_has_owner_reference() {
        let nw = sample_network();
        let sa = nw.create_owned_sa();
        assert_eq!(sa.metadata.name.as_deref(), Some("demo"));
        let owner_refs = sa.metadata.owner_references.as_ref().unwrap();
        assert_eq!(owner_refs.len(), 1);
        assert_eq!(owner_refs[0].name, "demo");
    }

    #[test]
    fn create_owned_role_binding_targets_service_account() {
        let nw = sample_network();
        let rb = nw.create_owned_role_binding("svc".into(), "role".into());
        let subject = rb.subjects.unwrap().pop().unwrap();
        assert_eq!(subject.name, "svc");
        assert_eq!(subject.namespace.as_deref(), Some("demo-ns"));
        assert_eq!(rb.role_ref.name, "role");
    }

    #[test]
    fn create_owned_router_populates_labels_and_prefix() {
        let nw = sample_network();
        let router = nw
            .create_owned_router("router-1", "node-a", None)
            .expect("router");
        assert_eq!(router.spec.prefix, "/example/router-1");
        assert_eq!(router.spec.node_name, "node-a");
        let labels = router.metadata.labels.unwrap();
        assert_eq!(labels.get(super::NETWORK_LABEL_KEY), Some(&"demo".into()));
        assert!(router.metadata.annotations.unwrap().contains_key("anno"));
    }

    #[test]
    fn create_owned_certificate_uses_router_name() {
        let nw = sample_network();
        let issuer = nw.spec.router_cert_issuer.as_ref().unwrap().clone();
        let cert = nw
            .create_owned_certificate("router-1", "router-1", &issuer)
            .expect("cert");
        assert_eq!(cert.spec.prefix, "/example/router-1/32=DV");
        assert_eq!(cert.spec.issuer.name, issuer.name);
        assert_eq!(cert.metadata.name.as_deref(), Some("router-1"));
    }
}
