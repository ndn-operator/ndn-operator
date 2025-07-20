use super::Context;
use crate::{cert_controller::{is_cert_valid, Certificate, CertificateSpec, IssuerRef}, helper::get_my_image, network_controller::{CertificateRef, Router, RouterSpec}, Error, Result};
use k8s_openapi::{
    api::{
        apps::v1::{DaemonSet, DaemonSetSpec},
        core::v1::{
            Container, ContainerPort, EnvVar, EnvVarSource, HostPathVolumeSource, ObjectFieldSelector, PodSpec, PodTemplateSpec, SecurityContext, ServiceAccount, Volume, VolumeMount
        }, rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject},
    },
    apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta},
};
use kube::{
    api::{Api, Patch, PatchParams, ResourceExt}, runtime::{
        controller::Action,
        events::{Event, EventType}, wait::await_condition,
    }, Client, CustomResource, Resource
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_with::skip_serializing_none;
use std::{collections::BTreeMap, sync::Arc};
use tracing::*;

pub static NETWORK_FINALIZER: &str = "network.named-data.net/finalizer";
pub static NETWORK_MANAGER_NAME: &str = "network-controller";
pub static NETWORK_LABEL_KEY: &str = "network.named-data.net/name";
pub static DS_LABEL_KEY : &str = "network.named-data.net/managed-by";
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
#[kube(group = "named-data.net", version = "v1alpha1", kind = "Network", derive="Default", namespaced, shortname = "nw")]
#[kube(status = "NetworkStatus")]
pub struct NetworkSpec {
    pub prefix: String,
    pub udp_unicast_port: i32,
    pub node_selector: Option<BTreeMap<String, String>>,
    pub ndnd: Option<NdndSpec>,
    pub operator: Option<OperatorSpec>,
    pub router_cert_issuer: Option<IssuerRef>,
    pub trust_anchors: Option<Vec<TrustAnchorRef>>,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrustAnchorRef {
    pub name: String,
    pub kind: String,
    pub namespace: Option<String>,
}

impl TrustAnchorRef {
    pub async fn get_cert_name(&self, client: &Client, default_ns: &str) -> Result<String> {
        match self.kind.as_str() {
            "Certificate" => {
                let api_cert = Api::<Certificate>::namespaced(client.clone(), &self.namespace.clone().unwrap_or(default_ns.to_string()));
                info!("Waiting for the router certificate to be valid...");
                let cert_valid = await_condition(
                    api_cert.clone(),
                    &self.name,
                    is_cert_valid()
                );
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), cert_valid).await.map_err(|e| Error::OtherError(format!("Timeout while waiting for certificate to be valid: {e}")))?;
                let cert = api_cert.get_status(&self.name).await.map_err(Error::KubeError)?;
                cert.status
                    .and_then(|s| s.cert.name)
                    .ok_or(Error::OtherError("Certificate name not found in status".to_string()))
            }
            _ => Err(Error::OtherError(format!("Unsupported trust anchor kind: {}", self.kind))),
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

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStatus {
    ds_created: Option<bool>,
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
        let role_binding_data = self.create_owned_role_binding(sa_data.name_any(), role_date.name_any());
        let operator_spec = &self.spec.operator;
        let ds_image = match operator_spec {
            Some(op) => op.image.clone(),
            None => get_my_image(ctx.client.clone()).await.ok().unwrap_or(OperatorSpec::default().image),
        };
        let ds_data = self.create_owned_daemonset(Some(ds_image), Some(sa_data.name_any()))?;
        // Create ServiceAccount
        let _sa = api_sa.patch(&self.name_any(), &serverside, &Patch::Apply(sa_data)).await.map_err(Error::KubeError)?;
        let _role = api_role.patch(&self.name_any(), &serverside, &Patch::Apply(role_date)).await.map_err(Error::KubeError)?;
        let _role_binding = api_role_binding.patch(&self.name_any(), &serverside, &Patch::Apply(role_binding_data)).await.map_err(Error::KubeError)?;
        // Create DaemonSet
        let ds = api_ds.patch(&self.name_any(), &serverside, &Patch::Apply(ds_data)).await.map_err(Error::KubeError)?;
        // Publish event
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "DaemonSetCreated".into(),
                    note: Some(format!("Created `{}` DaemonSet for `{}` Network", ds.name_any(), self.name_any())),
                    action: "Created".into(),
                    secondary: None,
                },
                &self.object_ref(&()),
            )
            .await
            .map_err(Error::KubeError)?;
        // Update the status of the Network
        let status = json!({
            "status": NetworkStatus {
                ds_created: Some(true),
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
        format!("{}/{}", HOST_SOCKET_ROOT_DIR, self.namespace().unwrap())
    }

    pub fn host_socket_path(&self) -> String {
        format!("{}/{}", self.host_socket_dir(), self.socket_file_name())
    }
    
    pub fn host_config_dir(&self) -> String {
        format!("{}/{}", HOST_CONFIG_ROOT_DIR, self.namespace().unwrap())
    }

    pub fn host_config_path(&self) -> String {
        format!("{}/{}", self.host_config_dir(), self.config_file_name())
    }

    pub fn host_keys_dir(&self) -> String {
        format!("{}/{}", HOST_KEYS_ROOT_DIR, self.namespace().unwrap())
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
                    resources: Some(vec!["networks".to_string()]),
                    verbs: vec![
                        "get".to_string(),
                    ],
                    ..PolicyRule::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["named-data.net".to_string()]),
                    resources: Some(vec!["routers/status".to_string()]),
                    verbs: vec!["update".to_string(), "patch".to_string()],
                    ..PolicyRule::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["named-data.net".to_string()]),
                    resources: Some(vec!["routers".to_string()]),
                    verbs: vec![
                        "get".to_string(),
                        "list".to_string(),
                        "watch".to_string(),
                        "create".to_string(),
                        "delete".to_string(),
                        "patch".to_string(),
                        "update".to_string(),
                    ],
                    ..PolicyRule::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["named-data.net".to_string()]),
                    resources: Some(vec!["certificates".to_string()]),
                    verbs: vec![
                        "list".to_string(),
                        "watch".to_string(),
                    ],
                    ..PolicyRule::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["named-data.net".to_string()]),
                    resources: Some(vec!["certificates/status".to_string()]),
                    verbs: vec![
                        "get".to_string(),
                    ],
                    ..PolicyRule::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["".to_string()]),
                    resources: Some(vec!["secrets".to_string()]),
                    verbs: vec![
                        "get".to_string(),
                    ],
                    ..PolicyRule::default()
                },
            ])
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
            subjects: Some(vec![
                Subject {
                    kind: "ServiceAccount".to_string(),
                    name: sa_name,
                    namespace: Some(self.namespace().unwrap()),
                    ..Subject::default()
                },
            ])
        }
    }

    fn create_owned_daemonset(&self, image: Option<String>, service_account: Option<String>) -> Result<DaemonSet> {
        let oref = self.controller_owner_ref(&())
            .ok_or(Error::OtherError("Failed to create owner reference".to_string()))?;
        let mut labels = BTreeMap::new();
        labels.insert(DS_LABEL_KEY.to_string(), self.name_any());
        let container_config_path = self.container_config_path();
        let container_socket_path = self.container_socket_path();
        let daemonset = DaemonSet {
            metadata: ObjectMeta {
                name: Some(self.name_any()),
                owner_references: Some(vec![oref]),
                labels: Some(labels.clone()),
                ..ObjectMeta::default()
            },
            spec: Some(DaemonSetSpec {
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    ..LabelSelector::default()
                },
                template: PodTemplateSpec {
                    metadata: Some(ObjectMeta {
                        labels: Some(labels.clone()),
                        ..ObjectMeta::default()
                    }),
                    spec: Some(PodSpec {
                        service_account_name: service_account,
                        host_network: Some(true),
                        dns_policy: Some("ClusterFirstWithHostNet".to_string()),
                        node_selector: self.spec.node_selector.clone(),
                        init_containers: Some(vec![Container {
                            name: "init".to_string(),
                            image: image.clone(),
                            command: vec!["/init".to_string(), "--output".to_string(), container_config_path.clone()].into(),
                            env: Some(vec![
                                EnvVar {
                                    name: "NDN_NETWORK_NAME".to_string(),
                                    value: Some(self.name_any()),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_UDP_UNICAST_PORT".to_string(),
                                    value: Some(self.spec.udp_unicast_port.to_string()),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "LOG".to_string(),
                                    value: Some("debug".to_string()),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_NETWORK_NAMESPACE".to_string(),
                                    value_from: Some(EnvVarSource {
                                        field_ref: Some(ObjectFieldSelector {
                                            field_path: "metadata.namespace".to_string(),
                                            ..ObjectFieldSelector::default()
                                        }),
                                        ..EnvVarSource::default()
                                    }),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    // Router name is equal to the pod name (pod_sync.rs#pod_apply)
                                    // TODO: use annotations?
                                    name: "NDN_ROUTER_NAME".to_string(),
                                    value_from: Some(EnvVarSource {
                                        field_ref: Some(ObjectFieldSelector {
                                            field_path: "metadata.name".to_string(),
                                            ..ObjectFieldSelector::default()
                                        }),
                                        ..EnvVarSource::default()
                                    }),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_NODE_NAME".to_string(),
                                    value_from: Some(EnvVarSource {
                                        field_ref: Some(ObjectFieldSelector {
                                            field_path: "spec.nodeName".to_string(),
                                            ..ObjectFieldSelector::default()
                                        }),
                                        ..EnvVarSource::default()
                                    }),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_SOCKET_PATH".to_string(),
                                    value: Some(container_socket_path.clone()),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_KEYS_DIR".to_string(),
                                    value: Some(self.container_keys_dir()),
                                ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_INSECURE".to_string(),
                                    value: match self.spec.router_cert_issuer {
                                        Some(_) => Some("false".to_string()),
                                        None => Some("true".to_string()),
                                    },
                                    ..EnvVar::default()
                                },
                            ]),
                            security_context: Some(SecurityContext {
                                privileged: Some(true),
                                ..SecurityContext::default()
                            }),
                            volume_mounts: Some(vec![
                                VolumeMount {
                                    name: "config".to_string(),
                                    mount_path: CONTAINER_CONFIG_DIR.to_string(),
                                    read_only: Some(false),
                                    ..VolumeMount::default()
                                },
                                VolumeMount {
                                    name: "keys".to_string(),
                                    mount_path: CONTAINER_KEYS_DIR.to_string(),
                                    read_only: Some(false),
                                    ..VolumeMount::default()
                                },
                            ]),
                            ..Container::default()
                        }]),
                        containers: vec![Container {
                            name: "network".to_string(),
                            image: Some(self.spec.ndnd.clone().unwrap_or_default().image),
                            command: vec!["/ndnd".to_string()].into(),
                            args: Some(vec!["daemon".to_string(), container_config_path.to_string()]),
                            security_context: Some(SecurityContext {
                                privileged: Some(true),
                                ..SecurityContext::default()
                            }),
                            ports: Some(vec![
                                ContainerPort {
                                    container_port: self.spec.udp_unicast_port,
                                    host_port: Some(self.spec.udp_unicast_port),
                                    protocol: Some("UDP".to_string()),
                                    ..ContainerPort::default()
                                },
                            ]),
                            env: Some(vec![
                                EnvVar {
                                    name: "NDN_CLIENT_TRANSPORT".to_string(),
                                    value: Some(format!("unix://{}", container_socket_path.clone())),
                                    ..EnvVar::default()
                                },
                            ]),
                            volume_mounts: Some(vec![
                                VolumeMount {
                                    name: "config".to_string(),
                                    mount_path: CONTAINER_CONFIG_DIR.to_string(),
                                    read_only: Some(true),
                                    ..VolumeMount::default()
                                },
                                VolumeMount {
                                    name: "run-ndnd".to_string(),
                                    mount_path: CONTAINER_SOCKET_DIR.to_string(),
                                    ..VolumeMount::default()
                                },
                                VolumeMount {
                                    name: "keys".to_string(),
                                    mount_path: CONTAINER_KEYS_DIR.to_string(),
                                    read_only: Some(true),
                                    ..VolumeMount::default()
                                },
                            ]),
                            ..Container::default()
                        },
                        Container {
                            name: "watch".to_string(),
                            image,
                            command: vec!["/sidecar".to_string()].into(),
                            env: Some(vec![
                                EnvVar {
                                    name: "NDN_NETWORK_NAME".to_string(),
                                    value: Some(self.name_any()),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "LOG".to_string(),
                                    value: Some("debug".to_string()),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_NETWORK_NAMESPACE".to_string(),
                                    value_from: Some(EnvVarSource {
                                        field_ref: Some(ObjectFieldSelector {
                                            field_path: "metadata.namespace".to_string(),
                                            ..ObjectFieldSelector::default()
                                        }),
                                        ..EnvVarSource::default()
                                    }),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    // Router name is equal to the pod name (pod_sync.rs#pod_apply)
                                    name: "NDN_ROUTER_NAME".to_string(),
                                    value_from: Some(EnvVarSource {
                                        field_ref: Some(ObjectFieldSelector {
                                            field_path: "metadata.name".to_string(),
                                            ..ObjectFieldSelector::default()
                                        }),
                                        ..EnvVarSource::default()
                                    }),
                                    ..EnvVar::default()
                                },
                                EnvVar {
                                    name: "NDN_CLIENT_TRANSPORT".to_string(),
                                    value: Some(format!("unix://{container_socket_path}")),
                                    ..EnvVar::default()
                                },
                            ]),
                            volume_mounts: Some(vec![
                                VolumeMount {
                                    name: "run-ndnd".to_string(),
                                    mount_path: CONTAINER_SOCKET_DIR.to_string(),
                                    ..VolumeMount::default()
                                },
                            ]),
                            ..Container::default()
                        }],
                        volumes: Some(vec![
                            Volume {
                                name: "config".to_string(),
                                host_path: Some(HostPathVolumeSource {
                                    path: self.host_config_dir(),
                                    type_: Some("DirectoryOrCreate".to_string())
                                }),
                                ..Volume::default()
                            },
                            Volume {
                                name: "run-ndnd".to_string(),
                                host_path: Some(HostPathVolumeSource {
                                    path: self.host_socket_dir(),
                                    type_: Some("DirectoryOrCreate".to_string())
                                }),
                                ..Volume::default()
                            },
                            Volume {
                                name: "keys".to_string(),
                                host_path: Some(HostPathVolumeSource {
                                    path: self.host_keys_dir(),
                                    type_: Some("DirectoryOrCreate".to_string())
                                }),
                                ..Volume::default()
                            },
                        ]),
                        ..PodSpec::default()
                    }),
                },
                ..DaemonSetSpec::default()
                
            }),
            ..Default::default()
        };
        Ok(daemonset)
    }

    pub fn create_owned_router(&self, name: &str, node_name: &str, cert: Option<Certificate>) -> Result<Router> {
        let oref = self.controller_owner_ref(&())
            .ok_or(Error::OtherError("Failed to create owner reference".to_string()))?;
        let router = Router {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: self.namespace(),
                owner_references: Some(vec![oref]),
                labels: {
                    let mut labels = self.labels().clone();
                    labels.extend(BTreeMap::from([(NETWORK_LABEL_KEY.to_string(), self.name_any())]));
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

    pub fn create_owned_certificate(&self, name: &str, router_name: &str, cert_issuer: &IssuerRef) -> Result<Certificate> {
        let oref = self.controller_owner_ref(&())
            .ok_or(Error::OtherError("Failed to create owner reference for certificate".to_string()))?;
        let cert = Certificate {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: self.namespace(),
                owner_references: Some(vec![oref]),
                labels: {
                    let mut labels = self.labels().clone();
                    labels.extend(BTreeMap::from([(NETWORK_LABEL_KEY.to_string(), self.name_any())]));
                    Some(labels)
                },
                annotations: Some(self.annotations().clone()),
                ..ObjectMeta::default()
            },
            spec: CertificateSpec {
                prefix: format!("{}/{}/32=DV", self.spec.prefix, router_name),
                issuer: cert_issuer.clone(),
                ..CertificateSpec::default()
            },
            ..Certificate::default()
        };
        Ok(cert)
    }
}
