use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub room_id: Option<u64>,
    pub buckets: Option<Vec<u64>>,
    pub exp: usize,
}

#[derive(Debug)]
pub struct AuthConfig {
    pub jwt_key: String,
    pub jwt_issuer: String,
    pub jwt_audience: String,
}

impl AuthConfig {
    pub fn from_env() -> Result<Self, anyhow::Error> {
        let jwt_key: String = std::env::var("JWT_KEY").unwrap_or_default();
        let jwt_issuer: String = std::env::var("JWT_ISSUER").unwrap_or_default();
        let jwt_audience = std::env::var("JWT_AUDIENCE").unwrap_or_default();

        if jwt_key.is_empty() {
            return Err(anyhow::anyhow!(
                "JWT_KEY environment variable is required for authentication"
            ));
        }

        if jwt_issuer.is_empty() {
            return Err(anyhow::anyhow!(
                "JWT_ISSUER environment variable is required for authentication"
            ));
        }

        if jwt_audience.is_empty() {
            return Err(anyhow::anyhow!(
                "JWT_AUDIENCE environment variable is required for authentication"
            ));
        }

        Ok(Self {
            jwt_key,
            jwt_issuer,
            jwt_audience,
        })
    }

    pub fn validate_jwt(&self, token: &str) -> Result<Claims, String> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.algorithms = vec![Algorithm::HS256];
        validation.set_required_spec_claims(&["exp"]);
        validation.validate_exp = true;

        let token_data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.jwt_key.as_ref()),
            &validation,
        )
        .map_err(|e| format!("invalid_jwt:{}", e))?;

        let claims = token_data.claims;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as usize;
        if claims.exp <= now {
            return Err("expired_jwt".to_string());
        }

        Ok(claims)
    }
}
