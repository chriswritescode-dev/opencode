use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

const VERIFIER_CHARSET: &[u8; 66] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn generate() -> Pkce {
    let verifier = rand::random::<[u8; 43]>()
        .iter()
        .map(|byte| VERIFIER_CHARSET[*byte as usize % VERIFIER_CHARSET.len()] as char)
        .collect::<String>();
    Pkce { challenge: challenge(&verifier), verifier }
}

pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}

pub fn state() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}
