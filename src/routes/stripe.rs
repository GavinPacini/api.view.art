use crate::AppState;
use axum::{
    extract::State,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::stripe_crypto::{StripeCrypto, CreateSessionParams, TransactionDetails};
use axum::http::StatusCode;
use serde_json::json;

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    source_currency: String,
    source_exchange_amount: u64,
    destination_network: String,
    destination_currency: String,
    wallet_address: String,
}

#[derive(Serialize)]
pub struct CreateSessionResponse {
    session_id: String,
    client_secret: String,
}

pub async fn create_crypto_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let params = CreateSessionParams {
        transaction_details: TransactionDetails {
            destination_currency: request.destination_currency,
            destination_exchange_amount: request.destination_exchange_amount,
            destination_network: request.destination_network,
        },
        customer_ip_address: None,
    };

    match state.stripe_crypto.create_session(params).await {
        Ok(session) => (
            StatusCode::OK,
            Json(json!({ "clientSecret": session.client_secret })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}