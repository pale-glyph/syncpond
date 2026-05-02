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

pub fn validate_jwt_claims(
    jwt_key: Option<&str>,
    jwt_issuer: Option<&str>,
    jwt_audience: Option<&str>,
    token: &str,
    reserved_bucket_id: u64,
) -> Result<Claims, String> {
    let jwt_key = jwt_key.ok_or_else(|| "no_jwt_key".to_string())?;

    let mut validation = Validation::new(Algorithm::HS256);
    validation.algorithms = vec![Algorithm::HS256];
    validation.set_required_spec_claims(&["exp"]);
    validation.validate_exp = true;

    if let Some(issuer) = jwt_issuer {
        validation.set_issuer(&[issuer]);
    }
    if let Some(audience) = jwt_audience {
        validation.set_audience(&[audience]);
    }

    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(jwt_key.as_ref()),
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

    if let Some(buckets) = &claims.buckets {
        if buckets.iter().any(|id| *id == reserved_bucket_id) {
            return Err("invalid_jwt_reserved_bucket".to_string());
        }
    }

    Ok(claims)
}
