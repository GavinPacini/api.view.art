use serde::{Deserialize, Serialize};
use reqwest::Client;
use anyhow::Result;

const STRIPE_API_BASE: &str = "https://api.stripe.com/v1";

pub struct StripeCrypto {
    client: Client,
    secret_key: String,
}

#[derive(Serialize, Default)]
pub struct CreateSessionParams {
    pub transaction_details: TransactionDetails,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customer_ip_address: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct TransactionDetails {
    pub destination_currency: String,
    pub destination_exchange_amount: u64,
    pub destination_network: String,
}

#[derive(Deserialize)]
pub struct SessionResponse {
    pub id: String,
    pub client_secret: String,
    // Add other fields as needed
}

impl StripeCrypto {
    pub fn new(secret_key: String) -> Self {
        Self {
            client: Client::new(),
            secret_key,
        }
    }

    pub async fn create_session(&self, params: CreateSessionParams) -> Result<SessionResponse> {
        let url = format!("{}/crypto/onramp_sessions", STRIPE_API_BASE);
        let response = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.secret_key))
            .json(&params)
            .send()
            .await?
            .json::<SessionResponse>()
            .await?;

        Ok(response)
    }
}