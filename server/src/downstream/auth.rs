use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub room: String,
    pub containers: Option<Vec<String>>,
    pub exp: usize,
}

#[derive(Debug, Deserialize)]
pub struct AuthMessage {
    #[serde(rename = "type")]
    pub typ: String,
    pub jwt: String,
    pub last_seen_counter: Option<u64>,
}

pub fn validate_jwt_claims(
    jwt_key: Option<&str>,
    jwt_issuer: Option<&str>,
    jwt_audience: Option<&str>,
    token: &str,
) -> Result<Claims, String> {
    let jwt_key = jwt_key.ok_or_else(|| "no_jwt_key".to_string())?;

    let mut validation = Validation::new(Algorithm::HS256);
    // Explicitly lock algorithms to prevent algorithm confusion attacks.
    validation.algorithms = vec![Algorithm::HS256];
    // Explicitly require `exp` and enforce expiration.
    validation.set_required_spec_claims(&["exp"]);
    validation.validate_exp = true;

    if let Some(issuer) = jwt_issuer {
        validation.set_issuer(&[issuer]);
    }
    if let Some(audience) = jwt_audience {
        validation.set_audience(&[audience]);
    }

    let token_data = decode::<Claims>(token, &DecodingKey::from_secret(jwt_key.as_ref()), &validation)
        .map_err(|e| format!("invalid_jwt:{}", e))?;

    let claims = token_data.claims;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as usize;
    if claims.exp <= now {
        return Err("expired_jwt".to_string());
    }

    if claims.room.trim().is_empty() {
        return Err("invalid_jwt_room".to_string());
    }

    if claims.sub != format!("room:{}", claims.room) {
        return Err("invalid_jwt_sub".to_string());
    }

    if claims.room.parse::<u64>().is_err() {
        return Err("invalid_jwt_room".to_string());
    }

    // Reject tokens that attempt to grant access to the reserved server-only container.
    if let Some(containers) = &claims.containers {
        if containers.iter().any(|c| c == "server_only") {
            return Err("invalid_jwt_reserved_container".to_string());
        }
    }

    Ok(claims)
}
