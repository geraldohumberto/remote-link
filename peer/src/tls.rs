use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_rustls::rustls::{self, ServerConfig, ClientConfig};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Caminho do certificado e chave privada
fn cert_path() -> (PathBuf, PathBuf) {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    (base.join(".remote-link-cert.pem"), base.join(".remote-link-key.pem"))
}

/// Gera ou carrega certificado auto-assinado
pub fn load_or_generate_cert() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let (cert_path, key_path) = cert_path();

    if cert_path.exists() && key_path.exists() {
        // Carrega existente
        let cert_pem = std::fs::read(&cert_path)?;
        let key_pem  = std::fs::read(&key_path)?;
        let certs = rustls_pemfile::certs(&mut cert_pem.as_slice())
            .collect::<Result<Vec<_>, _>>()?;
        let key = rustls_pemfile::private_key(&mut key_pem.as_slice())?
            .ok_or_else(|| anyhow::anyhow!("Chave privada não encontrada"))?;
        return Ok((certs, key));
    }

    // Gera novo certificado auto-assinado
    let cert = rcgen::generate_simple_self_signed(vec!["remote-link".to_string()])?;
    let cert_pem = cert.cert.pem();
    let key_pem  = cert.key_pair.serialize_pem();

    std::fs::write(&cert_path, &cert_pem)?;
    std::fs::write(&key_path,  &key_pem)?;

    let certs = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())?
        .ok_or_else(|| anyhow::anyhow!("Chave privada não encontrada"))?;

    Ok((certs, key))
}

/// Cria TlsAcceptor para o servidor
pub fn make_server_tls() -> Result<TlsAcceptor> {
    let (certs, key) = load_or_generate_cert()?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Cria TlsConnector para o cliente — aceita qualquer certificado (segurança via senha)
pub fn make_client_tls() -> Result<(TlsConnector, ServerName<'static>)> {
    // Aceita certificados auto-assinados — a segurança é garantida pela senha
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from("remote-link").unwrap();
    Ok((connector, server_name))
}

/// Verifica certificados sem validar CA — segurança por senha
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dh_params: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message, cert, dh_params,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dhs: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message, cert, dhs,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
