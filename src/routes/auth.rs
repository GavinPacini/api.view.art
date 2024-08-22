use {
    crate::{
        model::{GetAuth, VerifyAuth},
        routes::internal_error,
        AppState,
    },
    anyhow::anyhow,
    axum::{
        async_trait,
        extract::{FromRequestParts, State},
        http::{request::Parts, StatusCode},
        response::{IntoResponse, Response},
        Extension,
        Json,
        RequestPartsExt,
    },
    axum_extra::{
        headers::{authorization::Bearer, Authorization},
        TypedHeader,
    },
    bb8_redis::redis::{AsyncCommands, SetExpiry, SetOptions},
    chrono::Utc,
    ethers::types::Address,
    jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation},
    serde::{Deserialize, Serialize},
    serde_json::json,
    siwe::{generate_nonce, VerificationError},
};

/// 1 hour
const NONCE_EXPIRY: u64 = 60 * 60;

pub async fn get_nonce(
    state: State<AppState>,
    Json(GetAuth { address, chain_id }): Json<GetAuth>,
) -> impl IntoResponse {
    tracing::info!("getting nonce for {:?}", address);

    let nonce_key = format!("nonce:{:?}:{}", address, chain_id);

    match state.pool.get().await {
        Ok(mut conn) => match conn
            .get_ex::<&str, String>(&nonce_key, redis::Expiry::EX(NONCE_EXPIRY))
            .await
        {
            Ok(nonce) => (
                StatusCode::OK,
                json!({ "status": true, "nonce": nonce }).to_string(),
            ),
            Err(_) => {
                tracing::info!(
                    "No nonce found for address {}, generating new nonce",
                    address
                );
                let random_nonce = generate_nonce();

                match conn
                    .set_options::<&str, String, ()>(
                        &format!("nonce:{:?}:{}", address, chain_id),
                        random_nonce.clone(),
                        SetOptions::default().with_expiration(SetExpiry::EX(NONCE_EXPIRY)),
                    )
                    .await
                {
                    Ok(_) => (
                        StatusCode::OK,
                        json!({ "status": true, "nonce": random_nonce }).to_string(),
                    ),
                    Err(err) => {
                        tracing::error!("Error setting nonce for address {}: {:?}", address, err);
                        internal_error(anyhow!(err))
                    }
                }
            }
        },
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            internal_error(anyhow!(err))
        }
    }
}

pub async fn verify_auth(
    state: State<AppState>,
    Json(VerifyAuth { message, signature }): Json<VerifyAuth>,
) -> impl IntoResponse {
    let address = Address::from(message.address);
    let chain_id = message.chain_id;

    tracing::info!("verifying auth for {:?}", address);

    let nonce_key = format!("nonce:{:?}:{}", address, chain_id);

    match state.pool.get().await {
        Ok(mut conn) => match conn.get::<&str, String>(&nonce_key).await {
            Ok(nonce) => {
                let verify_error = match (
                    message.valid_now(),
                    message.domain == "view.art",
                    message.nonce == nonce,
                ) {
                    (false, _, _) => Err(VerificationError::Time),
                    (_, false, _) => Err(VerificationError::DomainMismatch),
                    (_, _, false) => Err(VerificationError::NonceMismatch),
                    _ => Ok(()),
                };

                if let Err(e) = verify_error {
                    return internal_error(anyhow!(e));
                }

                // TODO: Support verifying both eip191 and eip1271
                // Tried using the `verify` method which should try both, but it doesn't work
                match message
                    .verify_eip1271(signature.as_bytes(), &state.provider)
                    .await
                {
                    Ok(_) => {
                        tracing::debug!("signature verified");

                        match conn.del::<&str, ()>(&nonce_key).await {
                            Ok(_) => (),
                            Err(err) => {
                                tracing::error!(
                                    "Error deleting nonce for address {}: {:?}",
                                    address,
                                    err
                                );
                                return internal_error(anyhow!(err));
                            }
                        }

                        let claims = Claims {
                            address: Address::from(message.address),
                            // 30 days from now
                            exp: Utc::now().timestamp() + 30 * 24 * 60 * 60,
                        };

                        match encode(&Header::default(), &claims, &state.keys.encoding) {
                            Ok(token) => (
                                StatusCode::OK,
                                json!({ "status": true, "token": token }).to_string(),
                            ),
                            Err(err) => {
                                tracing::error!("Error encoding token: {:?}", err);
                                internal_error(anyhow!(err))
                            }
                        }
                    }
                    Err(err) => {
                        tracing::error!("Error verifying signature: {:#?}", err);
                        internal_error(anyhow!(err))
                    }
                }
            }
            Err(err) => {
                // if the nonce doesn't exist, return an error
                tracing::error!("Error getting nonce for address {}: {:?}", address, err);
                (
                    StatusCode::BAD_REQUEST,
                    json!({ "status": false, "error": "invalid nonce" }).to_string(),
                )
            }
        },
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            internal_error(anyhow!(err))
        }
    }
}

#[derive(Clone)]
pub struct Keys {
    encoding: EncodingKey,
    decoding: DecodingKey,
}

impl Keys {
    pub fn new(secret: &[u8]) -> Self {
        Self {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
        }
    }
}

#[async_trait]
impl<S> FromRequestParts<S> for Claims
where
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Extract the token from the authorization header
        let TypedHeader(Authorization(bearer)) = parts
            .extract::<TypedHeader<Authorization<Bearer>>>()
            .await
            .map_err(|_| AuthError::InvalidToken)?;

        let Extension(keys) = parts
            .extract::<Extension<Keys>>()
            .await
            .map_err(|_| AuthError::CouldNotAccessState)?;

        // Decode the user data
        let token_data = decode::<Claims>(bearer.token(), &keys.decoding, &Validation::default())
            .map_err(|_| AuthError::InvalidToken)?;

        Ok(token_data.claims)
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            AuthError::InvalidToken => (StatusCode::BAD_REQUEST, "Invalid token"),
            AuthError::CouldNotAccessState => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Could not access state")
            }
        };
        let body = Json(json!({
            "error": error_message,
        }));
        (status, body).into_response()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub address: Address,
    exp: i64,
}

#[derive(Debug)]
pub enum AuthError {
    InvalidToken,
    CouldNotAccessState,
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn get_team_api_key(secret: String) -> String {
        let keys = Keys::new(secret.as_bytes());
        let claims = Claims {
            address: Address::from([0; 20]),
            // 90 days from now
            exp: Utc::now().timestamp() + 90 * 24 * 60 * 60,
        };
        encode(&Header::default(), &claims, &keys.encoding).unwrap()
    }

    #[ignore]
    #[test]
    /// This test can be used to generate a token for the team API key
    /// You must set JWT_SECRET to the production secret
    /// cargo test test_generate_claim_for_0x0 -- --ignored --nocapture
    fn test_generate_claim_for_0x0() {
        let key = std::env::var("JWT_SECRET").expect("JWT_SECRET must be set");
        let token = get_team_api_key(key);

        dbg!(token);
    }
}
