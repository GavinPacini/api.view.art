use {
    crate::{
        model::{GetAuth, VerifyAuth},
        routes::internal_error,
        AppState,
    },
    alloy::primitives::Address,
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
    erc6492::verify_signature,
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
                    return (
                        StatusCode::BAD_REQUEST,
                        json!({ "status": false, "error": e.to_string() }).to_string(),
                    );
                }

                match verify_signature(
                    signature,
                    address,
                    message.to_string().as_bytes(),
                    state.provider.clone(),
                )
                .await
                {
                    Ok(verification) => {
                        if !verification.is_valid() {
                            tracing::debug!("signature invalid");
                            return (
                                StatusCode::BAD_REQUEST,
                                json!({ "status": false, "error": "signature invalid" })
                                    .to_string(),
                            );
                        }

                        tracing::debug!("signature valid");

                        if let Err(err) = conn.del::<&str, ()>(&nonce_key).await {
                            tracing::error!(
                                "Error deleting nonce for address {}: {:?}",
                                address,
                                err
                            );
                            return internal_error(anyhow!(err));
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

    #[tokio::test]
    async fn test_verify_auth() {
        let body = r#"{
    "message": "view.art wants you to sign in with your Ethereum account:\n0x3635a25d6c9b69C517AAeB17A9a30468202563fE\n\nTo finish connecting, you must sign a message in your wallet to verify that you are the owner of this account.\n\nURI: https://view.art\nVersion: 1\nChain ID: 8453\nNonce: EbyKsNBvyN3Kg6sMR\nIssued At: 2024-09-04T10:48:12.397Z",
    "signature": "0x000000000000000000000000ca11bde05977b3631167028862be2a173976ca110000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000028000000000000000000000000000000000000000000000000000000000000001e482ad56cb0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000200000000000000000000000000ba5ed0c6aa8c49038f819e587e2633c4a9f428a0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000e43ffba36f00000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000040719803d80885ac58a268822f7069b4944800653b33c4187182cc1dfb7ec46760b63a53353d81c02a45eb70880c05c3f3d1b724d6a024c262fc779c4d033977ab0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000026000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000001e0000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000c0000000000000000000000000000000000000000000000000000000000000012000000000000000000000000000000000000000000000000000000000000000170000000000000000000000000000000000000000000000000000000000000001c0271e6593325e5a24da85a977ab405b462f2e802ef0eaea719fef665be124bf49f2a4066f485308b73fcf3a3bdba9e03dd680f473218c575e45f7266b8aa77e0000000000000000000000000000000000000000000000000000000000000025f198086b2db17256731bc456673b96bcef23f51d1fbacdd7c4379ef65465572f1d0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000767b2274797065223a22776562617574686e2e676574222c226368616c6c656e6765223a22616e7549595a775631625071754c4f4378334d6570533455333044526975745f3169613837723466575159222c226f726967696e223a2268747470733a2f2f6b6579732e636f696e626173652e636f6d227d000000000000000000006492649264926492649264926492649264926492649264926492649264926492"
}"#;

        let _verify_auth: VerifyAuth = serde_json::from_str(body).unwrap();

        // TODO: finish test
    }
}
