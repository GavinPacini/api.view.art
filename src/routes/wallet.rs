use {
    crate::{model::ADDRESS_KEY, routes::internal_error, AppState},
    anyhow::anyhow,
    axum::{
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
    },
    bb8_redis::redis::AsyncCommands,
    serde_json::json,
    alloy::primitives::Address,
};

fn parse_truncated_address(address: &str) -> Result<Address, anyhow::Error> {
    if address.len() == 42 {
        // Full address, parse normally
        Address::parse_checksummed(address, None).map_err(|e| anyhow!(e))
    } else if address.len() == 14 && address.starts_with("0x") && address.contains("...") {
        // Truncated address, reconstruct and parse
        let parts: Vec<&str> = address.split("...").collect();
        if parts.len() != 2 || parts[0].len() != 6 || parts[1].len() != 4 {
            return Err(anyhow!("Invalid truncated address format"));
        }
        let full_address = format!("0x{}00000000000000000000{}", &parts[0][2..], parts[1]);
        Address::parse_checksummed(&full_address, None).map_err(|e| anyhow!(e))
    } else {
        Err(anyhow!("Invalid address format"))
    }
}

pub async fn get_channels(
    state: State<AppState>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    let address = match parse_truncated_address(&address) {
        Ok(address) => address,
        Err(err) => {
            tracing::error!("Error parsing address: {:?}", err);
            return internal_error(anyhow!(err));
        }
    };


    tracing::info!("get channels for address {}", address);

    let key = format!("{}:{}", ADDRESS_KEY, address);

    let channels: Vec<String> = match state.pool.get().await {
        Ok(mut conn) => match conn.smembers::<&str, Vec<String>>(&key).await {
            Ok(content) => content,
            Err(err) => {
                tracing::error!("Error getting content for address {}: {:?}", address, err);
                vec![]
            }
        },
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            vec![]
        }
    };

    (StatusCode::OK, json!({ "channels": channels }).to_string())
}
