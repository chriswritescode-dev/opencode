use std::sync::{Arc, LazyLock, OnceLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

pub fn client() -> reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .tls_backend_preconfigured(tls_config())
            .build()
            .expect("a client on a preconfigured TLS backend builds")
    });
    CLIENT.clone()
}

fn tls_config() -> rustls::ClientConfig {
    static CONFIG: LazyLock<rustls::ClientConfig> = LazyLock::new(|| {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PlatformVerifier { provider, verifier: OnceLock::new() }))
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        config
    });
    CONFIG.clone()
}

#[derive(Debug)]
struct PlatformVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
    verifier: OnceLock<Result<rustls_platform_verifier::Verifier, rustls::Error>>,
}

impl PlatformVerifier {
    fn get(&self) -> Result<&rustls_platform_verifier::Verifier, rustls::Error> {
        self.verifier
            .get_or_init(|| rustls_platform_verifier::Verifier::new(self.provider.clone()))
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl ServerCertVerifier for PlatformVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.get()?.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.get()?.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.get()?.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
