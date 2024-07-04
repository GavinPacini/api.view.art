use {
    crate::{
        model::{GetAuth, VerifyAuth},
        routes::internal_error,
        AppState,
    },
    anyhow::anyhow,
    axum::{extract::State, http::StatusCode, response::IntoResponse, Json},
    bb8_redis::redis::{AsyncCommands, SetExpiry, SetOptions},
    ethers::types::Address,
    serde_json::json,
    siwe::{generate_nonce, VerificationError},
};

const NONCE_EXPIRY: usize = 120;

pub async fn get_nonce(
    state: State<AppState>,
    Json(GetAuth { address, chain_id }): Json<GetAuth>,
) -> impl IntoResponse {
    let random_nonce = generate_nonce();

    tracing::debug!("address: {:?}", address);

    match state.pool.get().await {
        Ok(mut conn) => match conn
            .set_options::<&str, String, ()>(
                &format!("nonce:{:?}:{}", address, chain_id),
                random_nonce.to_string(),
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

    match state.pool.get().await {
        Ok(mut conn) => match conn
            .get::<&str, String>(&format!("nonce:{:?}:{}", address, chain_id))
            .await
        {
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

                // TODO: Support verifying both eipeip191 and eip1271
                // Tried using the `verify` method which should try both, but it doesn't work
                match message
                    .verify_eip1271(signature.as_bytes(), &state.provider)
                    .await
                {
                    Ok(_) => {
                        tracing::debug!("signature verified");
                        // TODO: generate token
                        // https://github.com/tokio-rs/axum/blob/main/examples/jwt/src/main.rs
                        // TODO: set token in redis
                        (
                            StatusCode::OK,
                            json!({ "status": true, "token": "test" }).to_string(),
                        )
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
