use {
    crate::{
        args::Args,
        model::{GetAuth, VerifyAuth},
        routes::internal_error,
        utils::keys::nonce_key,
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
        extract::cookie::{Cookie, CookieJar},
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

#[derive(Debug, Deserialize)]
pub struct PrivyClaims {
    #[serde(rename = "cr")]
    pub cr: String, // Assuming `cr` is a string, adjust if necessary

    #[serde(rename = "linked_accounts")]
    pub linked_accounts: Vec<LinkedAccount>, // We'll define `LinkedAccount` below

    #[serde(rename = "iss")]
    pub issuer: String,

    #[serde(rename = "iat")]
    pub issued_at: usize, // Issued at timestamp

    #[serde(rename = "aud")]
    pub app_id: String,

    #[serde(rename = "sub")]
    pub user_id: String,

    #[serde(rename = "exp")]
    pub expiration: usize,
}

#[derive(Debug, Deserialize)]
pub struct LinkedAccount {
    #[serde(rename = "type")]
    pub account_type: String,

    pub address: String,

    #[serde(rename = "chain_type")]
    pub chain_type: String,

    #[serde(rename = "wallet_client_type")]
    pub wallet_client_type: String,

    pub lv: usize,
}

impl PrivyClaims {
    pub fn valid(&self, app_id: &str) -> Result<(), anyhow::Error> {
        if self.app_id != app_id {
            return Err(anyhow!("aud claim must be your Privy App ID."));
        }
        if self.issuer != "privy.io" {
            return Err(anyhow!("iss claim must be 'privy.io'"));
        }
        if self.expiration < Utc::now().timestamp() as usize {
            return Err(anyhow!("Token is expired."));
        }
        if self.issued_at > Utc::now().timestamp() as usize {
            return Err(anyhow!("Token is issued in the future."));
        }
        if self.linked_accounts.is_empty() {
            return Err(anyhow!("No linked accounts found."));
        }

        Ok(())
    }
}

pub async fn get_nonce(
    state: State<AppState>,
    Json(GetAuth { address, chain_id }): Json<GetAuth>,
) -> impl IntoResponse {
    tracing::info!("getting nonce for {:?}", address);

    let nonce_key = nonce_key(&address, chain_id);

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
                        &nonce_key,
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

pub async fn verify_privy_auth(
    cookies: CookieJar,
) -> Result<impl IntoResponse, StatusCode> {
    let args = Args::load().await.map_err(|err| {
        tracing::error!("Error loading args: {:?}", err);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let privy_app_id = String::from(args.privy_app_id);  
    let privy_public_key = String::from(args.privy_public_key).replace("\\n", "\n");

    tracing::info!("Preparing to verify privy auth");

    // Retrieve the "privy-id-token" cookie
    let token = cookies.get("privy-id-token").map(|cookie| cookie.value()).ok_or_else(|| {
        tracing::error!("Missing privy-id-token");
        StatusCode::UNAUTHORIZED
    })?;

    // Create and configure the Validation instance
    let validation = Validation::new(jsonwebtoken::Algorithm::ES256);

    tracing::info!("Validation: {:?}", validation);

    let decoded = jsonwebtoken::decode::<PrivyClaims>(
        &token,
        &DecodingKey::from_ec_pem(privy_public_key.as_bytes()).map_err(|err| {
            tracing::error!("Failed to parse EC key: {:?}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?,
        &validation,
    )
    .map_err(|err| {
        tracing::error!("JWT verification error: {:?}", err);
        StatusCode::UNAUTHORIZED
    })?;

    let claims = decoded.claims;

    // Call the `valid` method to validate the claims
    claims.valid(&privy_app_id).map_err(|err| {
        tracing::error!("Claims validation error: {:?}", err);
        StatusCode::UNAUTHORIZED
    })?;

    // Log the decoded claims if valid
    tracing::info!("Decoded and validated JWT claims: {:?}", claims);

    // Log linked accounts
    for account in &claims.linked_accounts {
        tracing::info!("Linked account: {:?}", account);
    }

    Ok(StatusCode::OK)
}

pub async fn verify_auth(
    state: State<AppState>,
    Json(VerifyAuth { message, signature }): Json<VerifyAuth>,
) -> impl IntoResponse {
    let address = Address::from(message.address);
    let chain_id = message.chain_id;

    tracing::info!("verifying auth for {:?}", address);

    let nonce_key = nonce_key(&address, chain_id);

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
    /// You must set JWT_SECRET to the production secret and then run:
    /// cargo test test_generate_claim_for_0x0 -- --ignored --nocapture
    fn test_generate_claim_for_0x0() {
        let key = std::env::var("JWT_SECRET").expect("JWT_SECRET must be set");
        let token = get_team_api_key(key);

        dbg!(token);
    }
}
