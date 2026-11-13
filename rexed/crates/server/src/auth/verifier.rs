//! TLS proves possession and checks leaf validity; RE login checks per-user pin/CA trust.
use crate::transport::tls::rustls::{
    self,
    client::danger::HandshakeSignatureValid,
    pki_types::{CertificateDer, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use x509_parser::prelude::*;
#[derive(Debug)]
pub(super) struct CertificateVerifier;
pub(super) fn valid_leaf(der: &[u8], now: UnixTime) -> bool {
    let Ok((rest, cert)) = parse_x509_certificate(der) else {
        return false;
    };
    let Ok(time) = ASN1Time::from_timestamp(now.as_secs() as i64) else {
        return false;
    };
    rest.is_empty()
        && cert.validity().is_valid_at(time)
        && !cert.is_ca()
        && cert
            .key_usage()
            .is_ok_and(|usage| usage.is_none_or(|u| u.value.digital_signature()))
        && cert
            .extended_key_usage()
            .is_ok_and(|usage| usage.is_none_or(|u| u.value.client_auth))
}
pub(super) fn has_identity(der: &[u8], identity: &str) -> bool {
    parse_x509_certificate(der).ok().is_some_and(|(_, c)| {
        c.subject_alternative_name()
            .ok()
            .flatten()
            .is_some_and(|san| {
                san.value
                    .general_names
                    .iter()
                    .any(|name| matches!(name, GeneralName::URI(uri) if *uri == identity))
            })
    })
}
impl ClientCertVerifier for CertificateVerifier {
    fn client_auth_mandatory(&self) -> bool {
        false
    }
    // Empty hints let clients present pinned certificates with arbitrary issuers.
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        end: &CertificateDer<'_>,
        _chain: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        // The username arrives after TLS. Trust is checked against its lazily loaded
        // policy by Authentication before any business message is accepted.
        if valid_leaf(end.as_ref(), now) {
            return Ok(ClientCertVerified::assertion());
        }
        Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::ApplicationVerificationFailure,
        ))
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
