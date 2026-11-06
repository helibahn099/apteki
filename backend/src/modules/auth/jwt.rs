//! JWT access-токены, refresh-токены и хеширование секретов.

use chrono::Utc;
use hmac::{Hmac, Mac};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Claims {
    /// ID пользователя.
    pub sub: Uuid,
    /// ID сессии (проверяется в БД при каждом запросе — так работает logout).
    pub sid: Uuid,
    pub role: String,
    pub iat: i64,
    pub exp: i64,
    pub typ: String,
}

pub fn issue_access_token(
    secret: &str,
    user_id: Uuid,
    session_id: Uuid,
    role: &str,
    ttl_secs: i64,
) -> anyhow::Result<String> {
    let now = Utc::now().timestamp();
    let claims = Claims {
        sub: user_id,
        sid: session_id,
        role: role.to_string(),
        iat: now,
        exp: now + ttl_secs,
        typ: "access".into(),
    };
    Ok(encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?)
}

pub fn verify_access_token(secret: &str, token: &str) -> Option<Claims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 5;
    let data = decode::<Claims>(token, &DecodingKey::from_secret(secret.as_bytes()), &validation).ok()?;
    (data.claims.typ == "access").then_some(data.claims)
}

/// Случайный refresh-токен (256 бит, hex).
pub fn generate_refresh_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// SHA-256 хеш токена для хранения в БД.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// HMAC-SHA256(secret, phone:code) — в БД хранится только он, не сам код.
pub fn hash_otp(secret: &str, phone: &str, code: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC принимает ключ любой длины");
    mac.update(phone.as_bytes());
    mac.update(b":");
    mac.update(code.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Генерация цифрового кода заданной длины.
pub fn generate_otp(len: usize) -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..len).map(|_| char::from(b'0' + rng.gen_range(0..10u8))).collect()
}

/// Сравнение строк за постоянное время.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "unit-test-secret-unit-test-secret";

    #[test]
    fn access_token_roundtrip() {
        let (u, s) = (Uuid::new_v4(), Uuid::new_v4());
        let t = issue_access_token(SECRET, u, s, "user", 60).unwrap();
        let c = verify_access_token(SECRET, &t).unwrap();
        assert_eq!(c.sub, u);
        assert_eq!(c.sid, s);
        assert_eq!(c.role, "user");
    }

    #[test]
    fn rejects_wrong_secret_and_expired() {
        let t = issue_access_token(SECRET, Uuid::new_v4(), Uuid::new_v4(), "user", 60).unwrap();
        assert!(verify_access_token("another-secret", &t).is_none());
        let expired = issue_access_token(SECRET, Uuid::new_v4(), Uuid::new_v4(), "user", -120).unwrap();
        assert!(verify_access_token(SECRET, &expired).is_none());
    }

    #[test]
    fn otp_format_and_hash() {
        let code = generate_otp(6);
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        let h1 = hash_otp(SECRET, "+996555000111", "123456");
        assert_eq!(h1, hash_otp(SECRET, "+996555000111", "123456"));
        assert_ne!(h1, hash_otp(SECRET, "+996555000112", "123456"));
        assert!(!h1.contains("123456"));
    }

    #[test]
    fn refresh_tokens_unique_and_hashed() {
        let a = generate_refresh_token();
        let b = generate_refresh_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(hash_token(&a).len(), 64);
        assert_ne!(hash_token(&a), a);
    }

    #[test]
    fn constant_time_compare() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
