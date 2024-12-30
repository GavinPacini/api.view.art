use {
    crate::{
        routes::internal_error,
        utils::{
            keys::{
                channel_key,
                channel_view_key,
                item_stream_key,
                user_stream_key,
                user_view_key,
            },
            stream_helpers::get_channel_lifetime_views,
        },
        AppState,
    },
    anyhow::anyhow,
    axum::{
        extract::{ConnectInfo, Path, State},
        http::StatusCode,
        response::IntoResponse,
        Json,
    },
    bb8_redis::redis::{aio::ConnectionLike, AsyncCommands, Cmd, RedisError, RedisResult},
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
                tracing::error!(
                    "Error getting total views for channel {}: {:?}",
                    channel,
                    err
                );
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
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let user = addr.ip().to_string();
    let channel = channel.to_ascii_lowercase();
    tracing::info!("Log channel view for channel {}", channel);

    let channel_key = channel_key(&channel);
    let channel_view_key = channel_view_key(&channel);
    let user_view_key = user_view_key(&user, &channel);

    let mut conn = state.pool.get().await.map_err(|err| {
        tracing::error!("Error getting Redis connection: {:?}", err);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "Failed to connect to Redis" }).to_string(),
        )
    })?;

    // Check if the channel exists
    let exists: bool = conn
        .exists::<_, bool>(&channel_key)
        .await
        .map_err(|err| {
            tracing::error!("Error checking if channel exists: {:?}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "Redis error while checking existence" }).to_string(),
            )
        })?;

    if !exists {
        tracing::warn!("Channel {} does not exist", channel);
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({ "status": false, "error": "Channel does not exist" })).to_string(),
        ));
    }

    let ttl_seconds = 600; // 10 minutes in seconds

    let set_result: bool = redis::cmd("SET")
        .arg(&user_view_key)
        .arg(&channel)
        .arg("NX")
        .arg("EX")
        .arg(ttl_seconds)
        .query_async(&mut *conn)
        .await
        .map_err(|err| {
            tracing::error!("Error applying rate limit: {:?}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "Redis error while applying rate limit" }).to_string(),
            )
        })?;

    if set_result {
        tracing::info!("Set view and rate limit for user");
        let now = chrono::Utc::now().timestamp_millis();
        conn.ts_incrby(&channel_view_key, 1, Some(now))
            .await
            .map_err(|err| {
                tracing::error!("Error logging channel view for {}: {:?}", channel, err);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "Redis error while logging channel view" }).to_string(),
                )
            })?;

        // Check and update top N sorted sets for each time range
        let time_ranges = [
            ("top_channels_daily", 24 * 60 * 60 * 1000), // 24 hours
            ("top_channels_weekly", 7 * 24 * 60 * 60 * 1000), // 7 days
            ("top_channels_monthly", 30 * 24 * 60 * 60 * 1000), // 30 days
        ];

        for (sorted_set_key, retention) in time_ranges.iter() {
            let start_time = now - retention;
            let view_count: usize = redis::cmd("TS.RANGE")
                .arg(&channel_view_key)
                .arg(start_time)
                .arg(now)
                .query_async(&mut *conn)
                .await
                .map(|data_points: Vec<(i64, i64)>| data_points.len())
                .unwrap_or(0);

            let current_score: Option<f64> = conn.zscore(sorted_set_key, &channel).await.ok();

            if let Some(score) = current_score {
                if view_count as f64 > score {
                    conn.zadd(sorted_set_key, &channel, view_count as f64)
                        .await
                        .map_err(|err| {
                            tracing::error!(
                                "Error updating channel {} in sorted set {}: {:?}",
                                channel,
                                sorted_set_key,
                                err
                            );
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                json!({ "error": "Redis error while updating sorted set" })
                                    .to_string(),
                            )
                        })?;
                }
            } else {
                let scores: Vec<(String, f64)> = redis::cmd("ZRANGEBYSCORE")
                    .arg(sorted_set_key)
                    .arg("-inf")
                    .arg("+inf")
                    .arg("WITHSCORES")
                    .query_async(&mut *conn)
                    .await
                    .unwrap_or_default();

                let current_min_score = scores
                    .iter()
                    .rev()
                    .take(30)
                    .last()
                    .map(|(_, score)| *score)
                    .unwrap_or(0.0);

                if view_count as f64 > current_min_score {
                    conn.zadd(sorted_set_key, &channel, view_count as f64)
                        .await
                        .map_err(|err| {
                            tracing::error!(
                                "Error adding channel {} to sorted set {}: {:?}",
                                channel,
                                sorted_set_key,
                                err
                            );
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                json!({ "error": "Redis error while adding to sorted set" })
                                    .to_string(),
                            )
                        })?;
                }
            }

            conn.zremrangebyrank(sorted_set_key, 0, -31)
                .await
                .map_err(|err| {
                    tracing::error!("Error trimming sorted set {}: {:?}", sorted_set_key, err);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({ "error": "Redis error while trimming sorted set" }).to_string(),
                    )
                })?;
        }
    } else {
        tracing::info!("User already viewed channel within the last 10 minutes");
    }

    Ok((StatusCode::OK, json!({ "status": true }).to_string()))
}

pub async fn log_item_stream(
    state: State<AppState>,
    Path(item_id): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    let user = addr.ip().to_string();
    let item_id = item_id.to_ascii_lowercase();
    tracing::info!("Log item stream for item with id {}", item_id);

    let item_stream_key = item_stream_key(&item_id);
    let user_stream_key = user_stream_key(&user, &item_id);

    match state.pool.get().await {
        Ok(mut conn) => {
            let ttl_seconds = 600; // 10 minutes in seconds

            let set_result: Result<bool, RedisError> = redis::cmd("SET")
                .arg(&user_stream_key)
                .arg(&item_id)
                .arg("NX")
                .arg("EX")
                .arg(ttl_seconds)
                .query_async(&mut *conn)
                .await;

            match set_result {
                Ok(true) => {
                    tracing::info!("Set view and rate limit for user");
                    // Increment the count at the current timestamp by 1
                    if let Err(err) = conn
                        .ts_incrby(
                            &item_stream_key,
                            1,
                            Some(chrono::Utc::now().timestamp_millis()),
                        )
                        .await
                    {
                        tracing::error!(
                            "Error logging item stream for {}: {:?}",
                            item_stream_key,
                            err
                        );
                        return internal_error(anyhow!(err));
                    }
                }
                Ok(false) => {
                    tracing::info!("User already viewed item within the last 10 minutes");
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
