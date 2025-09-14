use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

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
    /// The prefix for the certificate, used for routing and naming conventions
    pub prefix: String,
    /// The issuer of the certificate, which can be another Certificate or an external issuer.
    /// Can reference itself if the certificate is self-signed
    pub issuer: IssuerRef,
    /// When the certificate should be renewed before it expires.
    /// This is a duration string, e.g., "7d2h" for 7 days and 2 hours.
    /// If not specified, defaults to 24 hours
    #[schemars(with = "Option<String>")]
    pub renew_before: Option<duration_string::DurationString>,
    /// Duration of the certificate validity (total lifetime)
    #[schemars(with = "Option<String>")]
    pub renew_interval: Option<duration_string::DurationString>,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IssuerRef {
    /// The name of the issuer, e.g., "router-cert"
    pub name: String,
    /// The kind of the issuer, e.g., "Certificate".
    /// Currently only "Certificate" is supported
    pub kind: String,
    /// The namespace of the issuer
    /// If not specified, the certificate's namespace will be used
    pub namespace: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertificateStatus {
    /// The status of the certificate key
    pub key: KeyStatus,
    /// The status of the certificate file
    pub cert: CertStatus,
    /// Indicates whether the key exists
    pub key_exists: bool,
    /// Indicates whether the cert exists
    pub cert_exists: bool,
    /// Indicates whether the certificate needs renewal
    pub needs_renewal: bool,
    /// Standard Kubernetes-style conditions for this certificate
    /// - Ready: key and cert are present and cert is valid
    /// - IssuerReady: issuer is resolved and usable (best-effort)
    /// - KeyReady: key secret available/usable (proxy by key_exists)
    /// - CertReady: cert available and valid
    /// - RenewalRequired: within renew window
    /// - Issuing: an issuance/renewal is in progress
    #[schemars(skip)]
    pub conditions: Option<Vec<Condition>>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    /// The parsed full NDN name of the key
    pub name: Option<String>,
    /// The signature type of the key, e.g., "ECDSA"
    pub sig_type: Option<String>,
    /// The name of the Secret that contains the key data
    pub secret: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertStatus {
    /// The parsed full NDN name of the certificate
    pub name: Option<String>,
    /// The signature type of the certificate, e.g., "ECDSA"
    pub sig_type: Option<String>,
    /// The name of the signer key used to sign the certificate
    pub signer_key: Option<String>,
    /// The time when the certificate was issued (RFC3339)
    pub issued_at: Option<String>,
    /// The time when the certificate is valid until (RFC3339)
    pub valid_until: Option<String>,
    /// The name of the Secret that contains the certificate data (and signer cert)
    pub secret: Option<String>,
    /// Indicates whether the certificate is currently valid (not expired)
    pub valid: bool,
}
