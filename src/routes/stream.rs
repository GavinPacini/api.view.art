use {
    
    crate::{
        routes::internal_error,
        utils::{
            keys::{channel_key, channel_view_key, item_stream_key, user_view_key},
            stream_helpers::get_channel_lifetime_views,
        },
        AppState,
    },
    anyhow::{anyhow},
    axum::{
        extract::{ConnectInfo, Path, State},
        http::StatusCode,
        response::{
            IntoResponse,
        },
        Json,
    },
    bb8_redis::redis::{aio::ConnectionLike, AsyncCommands, Cmd, RedisResult, RedisError},
    serde_json::json,
    std::{future::Future, net::SocketAddr, pin::Pin},
};

pub async fn get_channel_view_metrics(
    state: State<AppState>,
    Path(channel): Path<String>,
) -> impl IntoResponse {
    let channel = channel.to_ascii_lowercase();
    tracing::info!("Fetching all-time views for channel {}", channel);

    match state.pool.get().await {
        Ok(mut conn) => match get_channel_lifetime_views(&mut conn, &channel).await {
            Ok(total_views) => (
                StatusCode::OK,
                Json(json!({ "channel": channel, "total_views": total_views })),
            ),
            Err(err) => {
                tracing::error!("Error getting total views for channel {}: {:?}", channel, err);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "Failed to fetch total views" })),
                )
            }
        },
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to connect to Redis" })),
            )
        }
    }
}

pub async fn log_channel_view(
    state: State<AppState>,
    Path(channel): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    let user = addr.ip().to_string();
    let channel = channel.to_ascii_lowercase();
    tracing::info!("Log channel view for channel {}", channel);

    let channel_key = channel_key(&channel);
    let channel_view_key = channel_view_key(&channel);
    let user_view_key = user_view_key(&user, &channel);

    match state.pool.get().await {
        Ok(mut conn) => {
            // Check if the channel exists
            let exists: bool = match conn.exists::<_, bool>(&channel_key).await {
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

            let ttl_seconds = 600; // 10 minutes in seconds

            let set_result: Result<bool, RedisError> = redis::cmd("SET")
                .arg(&user_view_key)
                .arg(&channel)
                .arg("NX")
                .arg("EX")
                .arg(ttl_seconds)
                .query_async(&mut *conn)
                .await;

            match set_result {
                Ok(true) => {
                    tracing::info!("Set view and rate limit for user");
                    if let Err(err) = conn
                        .ts_incrby(&channel_view_key, 1, Some(chrono::Utc::now().timestamp_millis()))
                        .await
                    {
                        tracing::error!("Error logging page view for {}: {:?}", channel, err);
                        return internal_error(anyhow!(err));
                    }
                }
                Ok(false) => {
                    tracing::info!("User already viewed channel within the last 10 minutes");
                }
                Err(err) => {
                    tracing::error!("Error applying rate limit for {}: {:?}", user, err);
                    return internal_error(anyhow!(err));
                }
            }
        }
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            return internal_error(anyhow!(err));
        }
    }

    (StatusCode::OK, json!({ "status": true }).to_string())
}

pub async fn log_item_stream(
    state: State<AppState>,
    Path(item_id): Path<String>,
) -> impl IntoResponse {
    let item_id = item_id.to_ascii_lowercase();
    tracing::info!("Log item stream for item with id {}", item_id);

    let item_stream_key = item_stream_key(&item_id);

    match state.pool.get().await {
        Ok(mut conn) => {

            // Increment the count at the current timestamp by 1
            if let Err(err) = conn
                .ts_incrby(&item_stream_key, 1, Some(chrono::Utc::now().timestamp_millis()))
                .await
            {
                tracing::error!("Error logging item stream for {}: {:?}", item_stream_key, err);
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