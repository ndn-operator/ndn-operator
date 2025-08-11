use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use kube::CustomResource;

pub static CERTIFICATE_FINALIZER: &str = "certificate.named-data.net/finalizer";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[kube(
    group = "named-data.net",
    version = "v1alpha1",
    kind = "Certificate",
    derive = "Default",
    namespaced,
    shortname = "cert",
    doc = "Certificate is a custom resource for managing NDN certificates",
    printcolumn = r#"{"name":"Prefix","jsonPath":".spec.prefix","type":"string"}"#,
    printcolumn = r#"{"name":"Key","jsonPath":".status.key.name","type":"string"}"#,
    printcolumn = r#"{"name":"Cert","jsonPath":".status.cert.name","type":"string"}"#,
    printcolumn = r#"{"name":"Valid","jsonPath":".status.cert.valid","type":"boolean"}"#,
    printcolumn = r#"{"name":"Key Exists","jsonPath":".status.keyExists","type":"boolean"}"#,
    printcolumn = r#"{"name":"Cert Exists","jsonPath":".status.certExists","type":"boolean"}"#,
    printcolumn = r#"{"name":"Needs Renewal","jsonPath":".status.needsRenewal","type":"boolean"}"#,
    status = "CertificateStatus"
)]
pub struct CertificateSpec {
    pub prefix: String,
    pub issuer: IssuerRef,
    #[schemars(with = "Option<String>")]
    pub renew_before: Option<duration_string::DurationString>,
    #[schemars(with = "Option<String>")]
    pub renew_interval: Option<duration_string::DurationString>,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IssuerRef {
    pub name: String,
    pub kind: String,
    pub namespace: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertificateStatus {
    pub key: KeyStatus,
    pub cert: CertStatus,
    pub key_exists: bool,
    pub cert_exists: bool,
    pub needs_renewal: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub name: Option<String>,
    pub sig_type: Option<String>,
    pub secret: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertStatus {
    pub name: Option<String>,
    pub sig_type: Option<String>,
    pub signer_key: Option<String>,
    pub issued_at: Option<String>,
    pub valid_until: Option<String>,
    pub secret: Option<String>,
    pub valid: bool,
}
