use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String,          // user id or runner id
    pub role: Role,
    pub exp: usize,
    pub iat: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Runner,
}

pub struct TokenManager {
    secret: String,
}

impl TokenManager {
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
        }
    }

    pub fn create_token(&self, sub: impl Into<String>, role: Role, ttl_hours: i64) -> Result<String, jsonwebtoken::errors::Error> {
        let now = chrono::Utc::now();
        let claims = Claims {
            sub: sub.into(),
            role,
            iat: now.timestamp() as usize,
            exp: (now + chrono::Duration::hours(ttl_hours)).timestamp() as usize,
        };

        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
    }

    pub fn verify(&self, token: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
        decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &Validation::default(),
        )
        .map(|data| data.claims)
    }

    pub fn gen_uuid_token(&self) -> String {
        Uuid::new_v4().to_string()
    }
}
