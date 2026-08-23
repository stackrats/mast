//! APP_KEY generation. Laravel's `key:generate` produces `base64:` + the
//! base64 of 32 random bytes (AES-256-CBC key size); the format is stable
//! across framework versions, so Mast can mint one without PHP — which is the
//! point: the key is usually missing precisely when the app cannot run yet
//! (fresh clone, .env just copied from .env.example).

/// A fresh `base64:…` APP_KEY value from the OS entropy source.
pub fn generate_app_key() -> std::io::Result<String> {
    Ok(format!("base64:{}", base64(&random_bytes()?)))
}

#[cfg(unix)]
fn random_bytes() -> std::io::Result<[u8; 32]> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Windows adapter TODO: BCryptGenRandom (or the `getrandom` crate) when the
/// Windows port lands.
#[cfg(not(unix))]
fn random_bytes() -> std::io::Result<[u8; 32]> {
    Err(std::io::Error::other("APP_KEY generation is unix-only for now"))
}

/// Standard-alphabet base64 with padding — what PHP's `base64_encode` emits,
/// which is what Laravel's key parser expects. Hand-rolled because this crate
/// has no crypto dependency and the input is a fixed 32 bytes.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[cfg(unix)]
    #[test]
    fn generated_keys_have_laravel_shape_and_are_distinct() {
        let a = generate_app_key().unwrap();
        let b = generate_app_key().unwrap();
        for key in [&a, &b] {
            let encoded = key.strip_prefix("base64:").expect("prefix");
            // 32 bytes → 44 base64 chars, one pad char.
            assert_eq!(encoded.len(), 44, "{key}");
            assert!(encoded.ends_with('='), "{key}");
            assert!(
                encoded.trim_end_matches('=').chars().all(|c| c.is_ascii_alphanumeric()
                    || c == '+'
                    || c == '/'),
                "{key}"
            );
        }
        assert_ne!(a, b, "two keys from urandom must differ");
    }
}
