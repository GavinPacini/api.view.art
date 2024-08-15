use crate::AppState;
use axum::{
    extract::State,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::stripe_crypto::{StripeCrypto, CreateSessionParams, TransactionDetails};

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
    let stripe_crypto = StripeCrypto::new(state.stripe_secret_key.clone());

    let params = CreateSessionParams {
        transaction_details: TransactionDetails {
            supported_destination_networks: vec![request.destination_network.clone()],
            supported_destination_currencies: vec![request.destination_currency.clone()],
            source_currency: request.source_currency,
            source_exchange_amount: request.source_exchange_amount,
            destination_network: request.destination_network,
            destination_currency: request.destination_currency,
        },
        wallet_addresses: vec![request.wallet_address],
        custom_fees: None,
    };

    match stripe_crypto.create_session(params).await {
        Ok(session) => (
            StatusCode::OK,
            Json(CreateSessionResponse {
                session_id: session.id,
                client_secret: session.client_secret,
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}