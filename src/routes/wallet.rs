use {
    crate::{model::ADDRESS_KEY, AppState},
    alloy::primitives::Address,
    axum::{
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
    },
    bb8_redis::redis::AsyncCommands,
    serde_json::json,
};

pub async fn get_channels(
    state: State<AppState>,
    Path(address): Path<Address>,
) -> impl IntoResponse {
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
