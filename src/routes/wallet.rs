use {
    crate::{
        routes::internal_error,
        utils::{address_migration::migrate_addresses, keys::address_key},
        AppState,
    },
    alloy::primitives::Address,
    anyhow::anyhow,
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

    if let Err(err) = migrate_addresses(&address, state.pool.get().await).await {
        tracing::error!("Error migrating address key: {:?}", err);
        return internal_error(anyhow!(err));
    }

    let key = address_key(&address);

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
