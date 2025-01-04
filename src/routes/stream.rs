use {
    crate::{
        routes::internal_error,
        utils::{
            keys::{
                channel_key,
                channel_view_key,
                item_stream_key,
                top_channels_key,
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
    std::{collections::HashMap, future::Future, net::SocketAddr, pin::Pin},
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
    let exists: bool = conn.exists::<_, bool>(&channel_key).await.map_err(|err| {
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

    let set_result = if cfg!(test) {
        true // Skip rate limiting in tests
    } else {
        let ttl_seconds = 600; // 10 minutes in seconds

        redis::cmd("SET")
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
                    json!({ "error": "Redis error while applying rate limit"
                    })
                    .to_string(),
                )
            })?
    };

    if set_result {
        tracing::info!("Set view and rate limit for user");
        let now = chrono::Utc::now().timestamp_millis();

        // Increment the TimeSeries view count
        conn.ts_incrby(&channel_view_key, 1, Some(now))
            .await
            .map_err(|err| {
                tracing::error!("Error logging channel view for {}: {:?}", channel, err);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "Redis error while logging channel view" }).to_string(),
                )
            })?;

        let time_ranges: Vec<(&str, i64)> = vec![
            ("daily", 24 * 60 * 60),        // 24 hours
            // ("weekly", 7 * 24 * 60 * 60 * 1000),   // 7 days
            // ("monthly", 30 * 24 * 60 * 60 * 1000), // 30 days
        ];

        let top_channels_count = 10;

        for (time_range_key, retention) in time_ranges.iter() {
            // Calculate total views for the current time range
            let start_time = now - (retention * 1000);
            let _top_channels_key = top_channels_key(time_range_key);
            let view_count: Result<Vec<(i64, String)>, _> = redis::cmd("TS.RANGE")
                .arg(&channel_view_key)
                .arg(start_time)
                .arg(now)
                .query_async(&mut *conn)
                .await;

            match view_count {
                Ok(data_points) => {
                    // tracing::info!("Got {} data points", dataPoints.len());

                    let mut cursor = 0;
                    let mut channels_with_counts: HashMap<String, (usize, i64)> = HashMap::new();

                    // Get all top channels for the time range
                    loop {
                        let all_top_channel_key = format!("top_channels:{}:*", time_range_key);

                        let scan_result: Result<(u64, Vec<String>), _> = redis::cmd("SCAN")
                            .arg(cursor)
                            .arg("MATCH")
                            .arg(all_top_channel_key)
                            .query_async(&mut *conn)
                            .await;

                        match scan_result {
                            Ok((next_cursor, keys)) => {
                                for key in keys {
                                    let get_result: Result<Option<(i64, i64)>, _> =
                                        redis::cmd("TS.GET")
                                            .arg(&key)
                                            .query_async(&mut *conn)
                                            .await;

                                    if let Ok(Some((timestamp, value))) = get_result {
                                        let channel_name =
                                            key.trim_start_matches("channel_views:").to_string();
                                        channels_with_counts
                                            .insert(channel_name, (value as usize, timestamp));
                                    }
                                }

                                cursor = next_cursor;
                                if cursor == 0 {
                                    break;
                                }
                            }
                            Err(err) => {
                                tracing::error!("Failed to scan Redis keys: {:?}", err);
                            }
                        }
                    }

                    // Sort by view count (descending), then by timestamp (descending)
                    let mut sorted_channels: Vec<(String, usize, i64)> = channels_with_counts
                        .into_iter()
                        .map(|(name, (views, timestamp))| (name, views, timestamp))
                        .collect();
                    sorted_channels.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));

                    // Convert to final format needed by rest of code
                    let sorted_channels: Vec<(String, usize)> = sorted_channels
                        .into_iter()
                        .map(|(name, views, _)| (name, views))
                        .collect();

                    // Map sorted channels to the desired format
                    let formatted_channels: Vec<_> = sorted_channels
                        .clone()
                        .into_iter()
                        .take(top_channels_count)
                        .map(|(name, views)| json!({ "name": name, "views": views }))
                        .collect();

                    tracing::info!("Top channels: {:?}", formatted_channels);

                    let min_channel = sorted_channels
                        .last()
                        .map(|(name, count)| (name.clone(), *count))
                        .unwrap_or(("".to_string(), 0));
                    // tracing::info!("Min channel: {:?}", minChannel);

                    let current_channel_key =
                        format!("top_channels:{}:{}", time_range_key, channel);
                    // check if the channel is already in the top channels
                    let channel_exists = sorted_channels
                        .iter()
                        .any(|(name, _)| name == &current_channel_key);
                    // tracing::info!("Channel {} exists in top channels: {}", channel,
                    // channel_exists);

                    if channel_exists {
                        // tracing::info!("Channel already exists in top channels");
                        let _del_time_range_result: Result<(), _> = redis::cmd("DEL")
                            .arg(&current_channel_key)
                            .query_async(&mut *conn)
                            .await;
                    }

                    let should_add_channel = channel_exists
                        || sorted_channels.len() < top_channels_count
                        || data_points.len() > min_channel.1;
                    tracing::info!(
                        "Should add channel: {}, {}, {}, {}, {}, {}",
                        should_add_channel,
                        channel_exists,
                        sorted_channels.len(),
                        top_channels_count,
                        data_points.len(),
                        min_channel.1
                    );
                    if should_add_channel {
                        // tracing::info!("Adding channel to top channels");
                        for data_point in &data_points {
                            let data_timestamp = data_point.0;
                            let data_retention = data_timestamp + (retention * 1000) - now;
                            let data_view_count = &data_point.1;

                            // tracing::info!("Timestamp: {}, Retention: {}", dataTimestamp,
                            // dataRetention);

                            let _add_result: Result<(), _> = redis::cmd("TS.ADD")
                                .arg(&current_channel_key)
                                .arg(data_timestamp)
                                .arg(data_view_count)
                                .arg("RETENTION")
                                .arg(data_retention)
                                .arg("ON_DUPLICATE")
                                .arg("LAST")
                                .query_async(&mut *conn)
                                .await;

                            // tracing::info!("Add result: {:?}", addResult);
                        }
                    }

                    let should_delete = (!channel_exists && should_add_channel)
                        && sorted_channels.len() >= top_channels_count;
                    tracing::info!(
                        "Should delete: {}, {}, {}, {}",
                        should_delete,
                        channel_exists,
                        sorted_channels.len(),
                        top_channels_count
                    );
                    if should_delete {
                        let _del_time_range_result: Result<(), _> = redis::cmd("DEL")
                            .arg(&min_channel.0)
                            .query_async(&mut *conn)
                            .await;

                        // tracing::info!("Delete result: {:?}, {}",
                        // delTimeRangeResult, minChannel.0);
                    }
                }
                Err(err) => {
                    tracing::error!("Error getting view count: {:?}", err);
                }
            }
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
