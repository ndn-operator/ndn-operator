use std::{collections::BTreeMap, sync::Arc, time::Duration as StdDuration};

use chrono::{DateTime, Duration, Utc};
use duration_string::DurationString;
use k8s_openapi::api::core::v1::Secret;
use kube::{api::{Api, ObjectMeta, Patch, PatchParams, PostParams, ResourceExt}, Resource};
use kube::core::object::HasStatus;
use kube::runtime::{controller::Action, wait::Condition};
use serde_json::json;
use tracing::*;

use crate::{Error, Result, events_helper::emit_info};
use super::{Context};
use super::types::{Certificate, CertificateStatus};
use super::crypto::{generate_key, sign_cert, SignCertParams};

pub static CERTIFICATE_MANAGER_NAME: &str = "cert-controller";

impl Certificate {
    pub const SECRET_KEY: &str = "ndn.key";
    pub const CERT_KEY: &str = "ndn.cert";
    pub const SIGNER_CERT_KEY: &str = "signer.cert";

    pub async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        debug!("Reconciling Certificate {:?}", self.name_any());
        let ns = self.namespace().unwrap();
        let api_cert: Api<Certificate> = Api::namespaced(ctx.client.clone(), &ns);
        let api_secret: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);
        let status = self.status().cloned().unwrap_or_default();
        let mut action = Action::await_change();

        let new_status = match status.key_exists {
            true => match status.cert_exists {
                true => match status.needs_renewal {
                    true => self.create_cert(ctx.clone(), &ns, &status, &api_secret).await?,
                    false => {
                        let valid_until = self.valid_until(&status)?;
                        let requeue_duration = (valid_until - Utc::now())
                            .to_std()
                            .map_err(|e| Error::OtherError(e.to_string()))?;
                        action = Action::requeue(requeue_duration);
                        self.validate_cert(&status)?
                    }
                },
                false => self.create_cert(ctx.clone(), &ns, &status, &api_secret).await?,
            },
            false => self.create_key(ctx.clone(), &status, &api_secret).await?,
        };

        let serverside = PatchParams::apply(CERTIFICATE_MANAGER_NAME);
        let patch = Patch::Merge(json!({ "status": new_status }));
        api_cert.patch_status(self.name_any().as_str(), &serverside, &patch)
            .await
            .map_err(Error::KubeError)?;

        Ok(action)
    }

    fn valid_until(&self, status: &CertificateStatus) -> Result<DateTime<Utc>> {
        status.cert.valid_until.as_ref()
            .ok_or_else(|| Error::MissingAnnotation("validUntil".into()))
            .and_then(|s| DateTime::parse_from_rfc3339(s).map_err(Error::ParseError).map(|dt| dt.with_timezone(&Utc)))
    }

    fn renew_before(&self, status: &CertificateStatus) -> Result<DateTime<Utc>> {
        let renew_before: StdDuration = self.spec.renew_before
            .unwrap_or(DurationString::new(StdDuration::from_secs(60 * 60 * 24 * 7)))
            .into();
        Ok(self.valid_until(status)? - Duration::from_std(renew_before).map_err(|e| Error::OtherError(e.to_string()))?)
    }

    fn validate_cert(&self, status: &CertificateStatus) -> Result<CertificateStatus> {
        let mut new_status = status.clone();
        let valid_until = self.valid_until(status)?;
        let now = Utc::now();
        new_status.cert.valid = valid_until > now;
        let renew_before = self.renew_before(status)?;
        new_status.needs_renewal = renew_before <= now;
        Ok(new_status)
    }

    async fn create_cert(&self, ctx: Arc<Context>, ns: &str, status: &CertificateStatus, api_secret: &Api<Secret>) -> Result<CertificateStatus> {
        let mut new_status = status.clone();
        let issuer_ref = &self.spec.issuer;
    let (signer_key_secret_name, signer_cert_secret_name, self_signed) = match issuer_ref.kind.as_str() {
            "Certificate" => {
                let api_issuer: Api<Certificate> = Api::namespaced(ctx.client.clone(), issuer_ref.namespace.as_deref().unwrap_or(ns));
                let issuer = api_issuer.get_status(&issuer_ref.name).await.map_err(Error::KubeError)?;
                let issuer_status = issuer.status.as_ref().ok_or(Error::OtherError("Issuer status not found".to_string()))?;
                let key_secret_name = issuer_status.key_exists.then(|| issuer_status.key.secret.clone()).flatten();
                let cert_secret_name = issuer_status.cert_exists.then(|| issuer_status.cert.secret.clone()).flatten();
                let self_signed = issuer.metadata.uid.unwrap_or_default() == self.metadata.uid.clone().unwrap_or_default();
                (key_secret_name, cert_secret_name, self_signed)
            }
            _ => return Err(Error::OtherError(format!("Unsupported issuer kind: {}", issuer_ref.kind)))
        };

    let signer_key_secret_name = signer_key_secret_name.ok_or_else(|| Error::OtherError("Signer key secret name not found".to_string()))?;
    let signer_key_text = key_text_from_secret(api_secret, &signer_key_secret_name, Self::SECRET_KEY).await?;
        let my_key_secret_name = status.key.secret.clone().ok_or(Error::OtherError("Key secret not found".to_string()))?;
        let my_key_text = key_text_from_secret(api_secret, &my_key_secret_name, Self::SECRET_KEY).await?;
        let std_duration: StdDuration = self.spec.renew_interval
            .unwrap_or(DurationString::new(StdDuration::from_secs(60 * 60 * 24 * 30)))
            .into();
        let cert_info = sign_cert(&signer_key_text, &my_key_text, &SignCertParams {
            start: Some(Utc::now()),
            end: Some(Utc::now() + Duration::from_std(std_duration).unwrap_or_default()),
            issuer: Some(self.spec.issuer.name.clone()),
            info: None,
        })?;

        let signer_cert_text = match signer_cert_secret_name {
            Some(secret_name) => key_text_from_secret(api_secret, &secret_name, Self::CERT_KEY).await?,
            None if self_signed => cert_info.cert_text.clone(),
            None => return Err(Error::OtherError("Signer cert secret not found".to_string())),
        };

        let cert_secret_name = format!("{}-cert", self.name_any());
        let cert_data = self.create_owned_secret(cert_secret_name.clone(), &BTreeMap::from([
            (Self::CERT_KEY.to_string(), cert_info.cert_text),
            (Self::SIGNER_CERT_KEY.to_string(), signer_cert_text),
        ]));
        let serverside = PatchParams::apply(CERTIFICATE_MANAGER_NAME);
        let cert_secret = api_secret.patch(&cert_secret_name, &serverside, &Patch::Apply(cert_data)).await.map_err(Error::KubeError)?;

        emit_info(&ctx.recorder, self, "CertCreated", "Created", Some(format!("Created `{}` Cert for `{}` Certificate", cert_secret.name_any(), self.name_any()))).await;

        new_status.cert.name = Some(cert_info.name);
        new_status.cert.sig_type = Some(cert_info.sig_type);
        new_status.cert.signer_key = Some(cert_info.signer_key);
        new_status.cert.issued_at = Some(cert_info.validity.0.to_rfc3339());
        new_status.cert.valid_until = Some(cert_info.validity.1.to_rfc3339());
        new_status.cert.secret = Some(cert_secret.name_any());
        new_status.cert.valid = true;
        new_status.cert_exists = true;
        Ok(new_status)
    }

    async fn create_key(&self, ctx: Arc<Context>, status: &CertificateStatus, api_secret: &Api<Secret>) -> Result<CertificateStatus> {
        let key_info = generate_key(&self.spec.prefix)?;
        let secret_name = format!("{}-key", self.name_any());
        let secret_data = self.create_owned_secret(secret_name.clone(), &BTreeMap::from([(Self::SECRET_KEY.to_string(), key_info.key_text)]));
        let secret = api_secret.create(&PostParams::default(), &secret_data).await.map_err(Error::KubeError)?;
        emit_info(&ctx.recorder, self, "KeyCreated", "Created", Some(format!("Created `{}` Key for `{}` Certificate", secret.name_any(), self.name_any()))).await;
        let mut new_status = status.clone();
        new_status.key.name = Some(key_info.name);
        new_status.key.sig_type = Some(key_info.sig_type);
        new_status.key.secret = Some(secret.name_any());
        new_status.key_exists = true;
        Ok(new_status)
    }

    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        emit_info(&ctx.recorder, self, "DeleteRequested", "Deleting", Some(format!("Delete `{}`", self.name_any()))).await;
        Ok(Action::await_change())
    }

    fn create_owned_secret(&self, name: String, data: &BTreeMap<String, String>) -> Secret {
        let oref = self.controller_owner_ref(&()).unwrap();
        Secret { metadata: ObjectMeta { name: Some(name), owner_references: Some(vec![oref]), ..ObjectMeta::default() }, string_data: Some(data.clone()), ..Secret::default() }
    }
}

pub fn is_cert_valid() -> impl Condition<Certificate> { |obj: Option<&Certificate>| obj.and_then(|c| c.status.clone()).unwrap_or_default().cert.valid }

async fn key_text_from_secret(api_secret: &Api<Secret>, secret_name: &str, key: &str) -> Result<String> {
    use crate::helper::{decode_secret, Decoded};
    let key_secret = api_secret.get(secret_name).await.map_err(Error::KubeError)?;
    let key_secret_data = decode_secret(&key_secret);
    let key_data = key_secret_data.get(key).ok_or(Error::OtherError("Key data not found".to_string()))?;
    match key_data { Decoded::Utf8(s) => Ok(s.clone()), Decoded::Bytes(_) => Err(Error::OtherError("Key data is not UTF-8".to_string())) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert_controller::types::{CertificateSpec, IssuerRef, CertificateStatus, KeyStatus, CertStatus};
    use chrono::Utc;
    use duration_string::DurationString;

    fn base_cert(valid_until: DateTime<Utc>) -> Certificate {
        let mut cert = Certificate::new("test-cert", CertificateSpec {
            prefix: "/test".into(),
            issuer: IssuerRef { name: "issuer".into(), kind: "Certificate".into(), namespace: None },
            renew_before: None, // default 7d
            renew_interval: None,
        });
        cert.status = Some(CertificateStatus {
            key: KeyStatus { name: Some("k".into()), sig_type: Some("ECDSA".into()), secret: Some("ksec".into()) },
            cert: CertStatus { name: Some("c".into()), sig_type: Some("ECDSA".into()), signer_key: Some("sk".into()), issued_at: Some(Utc::now().to_rfc3339()), valid_until: Some(valid_until.to_rfc3339()), secret: Some("csec".into()), valid: true },
            key_exists: true,
            cert_exists: true,
            needs_renewal: false,
        });
        cert
    }

    #[test]
    fn validate_cert_not_in_renew_window() {
        let valid_until = Utc::now() + Duration::days(14);
        let cert = base_cert(valid_until);
        let status = cert.status.clone().unwrap();
        let new_status = cert.validate_cert(&status).expect("validate");
        assert!(new_status.cert.valid);
        assert!(!new_status.needs_renewal, "Should not need renewal yet");
    }

    #[test]
    fn validate_cert_in_renew_window() {
        let valid_until = Utc::now() + Duration::days(5); // within default 7d renew_before
        let cert = base_cert(valid_until);
        let status = cert.status.clone().unwrap();
        let new_status = cert.validate_cert(&status).expect("validate");
        assert!(new_status.cert.valid);
        assert!(new_status.needs_renewal, "Should need renewal when inside window");
    }

    #[test]
    fn validate_cert_expired() {
        let valid_until = Utc::now() - Duration::hours(1);
        let cert = base_cert(valid_until);
        let status = cert.status.clone().unwrap();
        let new_status = cert.validate_cert(&status).expect("validate");
        assert!(!new_status.cert.valid, "Expired cert should be marked invalid");
        assert!(new_status.needs_renewal, "Expired cert needs renewal");
    }

    #[test]
    fn validate_cert_custom_renew_before() {
        // Set custom renew_before of 2 days
        let mut cert = Certificate::new("test-cert", CertificateSpec {
            prefix: "/test".into(),
            issuer: IssuerRef { name: "issuer".into(), kind: "Certificate".into(), namespace: None },
            renew_before: Some(DurationString::new(std::time::Duration::from_secs(60*60*24*2))),
            renew_interval: None,
        });
        let valid_until = Utc::now() + Duration::hours(47); // < 2 days
        cert.status = Some(CertificateStatus {
            key: KeyStatus { name: Some("k".into()), sig_type: Some("ECDSA".into()), secret: Some("ksec".into()) },
            cert: CertStatus { name: Some("c".into()), sig_type: Some("ECDSA".into()), signer_key: Some("sk".into()), issued_at: Some(Utc::now().to_rfc3339()), valid_until: Some(valid_until.to_rfc3339()), secret: Some("csec".into()), valid: true },
            key_exists: true,
            cert_exists: true,
            needs_renewal: false,
        });
        let status = cert.status.clone().unwrap();
        let new_status = cert.validate_cert(&status).expect("validate");
        assert!(new_status.needs_renewal, "Inside custom renew window");
    }
}
