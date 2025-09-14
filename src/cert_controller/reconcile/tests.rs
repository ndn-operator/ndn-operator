use super::*;
use crate::cert_controller::types::{
    CertStatus, CertificateSpec, CertificateStatus, IssuerRef, KeyStatus,
};
use chrono::Utc;
use duration_string::DurationString;

fn base_cert(valid_until: DateTime<Utc>) -> Certificate {
    let mut cert = Certificate::new(
        "test-cert",
        CertificateSpec {
            prefix: "/test".into(),
            issuer: IssuerRef {
                name: "issuer".into(),
                kind: "Certificate".into(),
                namespace: None,
            },
            renew_before: None, // default 7d
            renew_interval: None,
        },
    );
    cert.status = Some(CertificateStatus {
        key: KeyStatus {
            name: Some("k".into()),
            sig_type: Some("ECDSA".into()),
            secret: Some("ksec".into()),
        },
        cert: CertStatus {
            name: Some("c".into()),
            sig_type: Some("ECDSA".into()),
            signer_key: Some("sk".into()),
            issued_at: Some(Utc::now().to_rfc3339()),
            valid_until: Some(valid_until.to_rfc3339()),
            secret: Some("csec".into()),
            valid: true,
        },
        key_exists: true,
        cert_exists: true,
        needs_renewal: false,
        conditions: None,
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
    assert!(
        new_status.needs_renewal,
        "Should need renewal when inside window"
    );
}

#[test]
fn validate_cert_expired() {
    let valid_until = Utc::now() - Duration::hours(1);
    let cert = base_cert(valid_until);
    let status = cert.status.clone().unwrap();
    let new_status = cert.validate_cert(&status).expect("validate");
    assert!(
        !new_status.cert.valid,
        "Expired cert should be marked invalid"
    );
    assert!(new_status.needs_renewal, "Expired cert needs renewal");
}

#[test]
fn validate_cert_custom_renew_before() {
    // Set custom renew_before of 2 days
    let mut cert = Certificate::new(
        "test-cert",
        CertificateSpec {
            prefix: "/test".into(),
            issuer: IssuerRef {
                name: "issuer".into(),
                kind: "Certificate".into(),
                namespace: None,
            },
            renew_before: Some(DurationString::new(std::time::Duration::from_secs(
                60 * 60 * 24 * 2,
            ))),
            renew_interval: None,
        },
    );
    let valid_until = Utc::now() + Duration::hours(47); // < 2 days
    cert.status = Some(CertificateStatus {
        key: KeyStatus {
            name: Some("k".into()),
            sig_type: Some("ECDSA".into()),
            secret: Some("ksec".into()),
        },
        cert: CertStatus {
            name: Some("c".into()),
            sig_type: Some("ECDSA".into()),
            signer_key: Some("sk".into()),
            issued_at: Some(Utc::now().to_rfc3339()),
            valid_until: Some(valid_until.to_rfc3339()),
            secret: Some("csec".into()),
            valid: true,
        },
        key_exists: true,
        cert_exists: true,
        needs_renewal: false,
        conditions: None,
    });
    let status = cert.status.clone().unwrap();
    let new_status = cert.validate_cert(&status).expect("validate");
    assert!(new_status.needs_renewal, "Inside custom renew window");
}

#[test]
fn ready_true_when_key_and_cert_ready() {
    let valid_until = Utc::now() + Duration::days(30);
    let cert = base_cert(valid_until);
    // Compute availability which will set Ready
    let mut status = cert.status.clone().unwrap();
    super::set_availability_conditions(&cert, &mut status);
    let ready = status
        .conditions
        .unwrap_or_default()
        .into_iter()
        .find(|c| c.type_ == "Ready")
        .expect("Ready condition present");
    assert_eq!(ready.status, "True");
}

#[test]
fn ready_false_when_cert_invalid() {
    // expired cert
    let cert = base_cert(Utc::now() - Duration::hours(1));
    let mut status = cert.status.clone().unwrap();
    // Ensure validate_cert marks invalid
    status = cert.validate_cert(&status).expect("validate");
    super::set_availability_conditions(&cert, &mut status);
    let ready = status
        .conditions
        .unwrap_or_default()
        .into_iter()
        .find(|c| c.type_ == "Ready")
        .expect("Ready condition present");
    assert_eq!(ready.status, "False");
}

#[test]
fn ready_independent_of_renewal_required() {
    // inside renew window but still valid
    let valid_until = Utc::now() + Duration::days(5); // within default 7d window
    let cert = base_cert(valid_until);
    let status = cert.status.clone().unwrap();
    let mut new_status = cert.validate_cert(&status).expect("validate");
    // needs_renewal should be true now, but Ready should remain true as long as key/cert are good
    assert!(new_status.needs_renewal);
    super::set_availability_conditions(&cert, &mut new_status);
    let ready = new_status
        .conditions
        .unwrap_or_default()
        .into_iter()
        .find(|c| c.type_ == "Ready")
        .expect("Ready condition present");
    assert_eq!(ready.status, "True");
}
