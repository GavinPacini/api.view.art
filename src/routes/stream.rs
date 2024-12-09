use {
    
    crate::{
        routes::internal_error,
        utils::{
            keys::{channel_key},
        },
        AppState,
    },
    anyhow::{anyhow},
    axum::{
        extract::{Path, State},
        http::StatusCode,
        response::{
            IntoResponse,
        },
        Json,
    },
    bb8_redis::redis::{aio::ConnectionLike, AsyncCommands, Cmd, RedisResult},
    serde_json::json,
    std::{future::Future, pin::Pin},

};

pub async fn log_channel_view(
    state: State<AppState>,
    Path(channel): Path<String>,
) -> impl IntoResponse {
    let channel = channel.to_ascii_lowercase();
    tracing::info!("Log page view for channel {}", channel);

    let channel_key = channel_key(&channel);
    let page_view_key = format!("page_views:{}", channel); // Key for time-series data

    match state.pool.get().await {
        Ok(mut conn) => {
            // Check if the channel exists
            let exists: bool = match conn.exists::<&str, bool>(&channel_key).await {
                Ok(exists) => exists,
                Err(err) => {
                    tracing::error!("Error checking if channel exists: {:?}", err);
                    return internal_error(anyhow!(err));
                }
            };

            if !exists {
                tracing::warn!("Channel {} does not exist", channel);
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "status": false, "error": "Channel does not exist" })).to_string(),
                );
            }

            // Increment the count at the current timestamp by 1
            if let Err(err) = conn
                .ts_incrby(&page_view_key, 1, Some(chrono::Utc::now().timestamp_millis()))
                .await
            {
                tracing::error!("Error logging page view for {}: {:?}", channel, err);
                return internal_error(anyhow!(err));
            }
        }
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            return internal_error(anyhow!(err));
        }
    }

    (StatusCode::OK, json!({ "status": true }).to_string())
}

pub trait TimeSeriesCommands: Send {
    fn ts_incrby<'a>(
        &'a mut self,
        key: &'a str,
        increment: i64,
        timestamp: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = RedisResult<()>> + Send + 'a>>;
}

impl<T: ConnectionLike + Send> TimeSeriesCommands for T {
    fn ts_incrby<'a>(
        &'a mut self,
        key: &'a str,
        increment: i64,
        timestamp: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = RedisResult<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut cmd = Cmd::new();
            cmd.arg("TS.INCRBY").arg(key).arg(increment);

            if let Some(ts) = timestamp {
                cmd.arg("TIMESTAMP").arg(ts);
            }

            cmd.query_async(self).await
        })
    }
}