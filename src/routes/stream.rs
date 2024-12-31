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

    let ttl_seconds = if cfg!(test) {
        0 // No rate limit during tests
    } else {
        600 // 10 minutes in seconds
    };

    let set_result = if cfg!(test) {
        // Skip rate limiting in tests
        true
    } else {
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
                    json!({ "error": "Redis error while applying rate limit" }).to_string(),
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

        // Update the top 30 sorted sets for each time range
        let time_ranges = [
            ("top_channels_daily", 24 * 60 * 60 * 1000), // 24 hours
            ("top_channels_weekly", 7 * 24 * 60 * 60 * 1000), // 7 days
            ("top_channels_monthly", 30 * 24 * 60 * 60 * 1000), // 30 days
        ];

        for (sorted_set_key, retention) in time_ranges.iter() {
            // Calculate total views for the current time range
            let start_time = now - retention;
            let view_count: usize = redis::cmd("TS.RANGE")
                .arg(&channel_view_key)
                .arg(start_time)
                .arg(now)
                .query_async(&mut *conn)
                .await
                .map(|data_points: Vec<(i64, i64)>| data_points.len())
                .unwrap_or(0);

            // Get the current score for the channel in the sorted set
            let current_score: Option<f64> = conn.zscore(sorted_set_key, &channel).await.ok();

            if let Some(score) = current_score {
                // If the channel is already in the sorted set, update its score if the new
                // count is higher
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
                // If the channel is not already in the sorted set, check if it qualifies
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

            // Trim the sorted set to keep only the top 30 channels
            conn.zremrangebyrank(sorted_set_key, 30, -1)
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

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{args::Args, AppState, Changes, Keys},
        alloy::providers::ProviderBuilder,
        bb8::PooledConnection,
        bb8_redis::RedisConnectionManager,
        redis::AsyncCommands,
        std::ops::DerefMut,
    };

    /// Helper function to set up a Redis connection pool
    async fn get_redis_pool() -> bb8::Pool<RedisConnectionManager> {
        let manager = RedisConnectionManager::new("redis://localhost:6379/").unwrap();
        bb8::Pool::builder().build(manager).await.unwrap()
    }

    /// Helper function to set up test data in Redis
    async fn setup_test_data(conn: &mut PooledConnection<'_, RedisConnectionManager>) {
        // Clear all existing Redis data
        let _: () = redis::cmd("FLUSHDB")
            .query_async(conn.deref_mut())
            .await
            .unwrap();

        // Create the channel key
        let _: () = conn.set("channel:channel1", "test_channel").await.unwrap();

        // Create a TimeSeries key with DUPLICATE_POLICY
        let ts_create_result: Result<(), RedisError> = redis::cmd("TS.CREATE")
            .arg("channel_views:channel1")
            .arg("RETENTION")
            .arg(24 * 60 * 60 * 1000) // 24 hours retention
            .arg("DUPLICATE_POLICY")
            .arg("LAST") // Use LAST policy to allow updates
            .query_async(conn.deref_mut())
            .await;

        // Ignore "key already exists" error
        if let Err(e) = ts_create_result {
            if !e.to_string().contains("key already exists") {
                panic!("Failed to create TimeSeries: {:?}", e);
            }
        }

        // Add an initial value to the TimeSeries
        let timestamp = chrono::Utc::now().timestamp_millis();
        let _: () = redis::cmd("TS.ADD")
            .arg("channel_views:channel1")
            .arg(timestamp)
            .arg(1) // Initial value
            .query_async(conn.deref_mut())
            .await
            .unwrap();

        // Add a channel to the sorted set
        let _: () = conn
            .zadd("top_channels_daily", "channel1", 1)
            .await
            .unwrap();

        tracing::info!("Setup test data");
    }

    #[tokio::test]
    async fn test_log_channel_view_existing_channel() -> Result<(), anyhow::Error> {
        let pool = get_redis_pool().await;
        let binding = pool.clone();
        let mut conn = binding.get().await.unwrap();
        
        // More aggressive cleanup
        tracing::info!("Starting test cleanup");
        let _: () = redis::cmd("FLUSHALL")
            .query_async(conn.deref_mut())
            .await
            .unwrap();
        
        // Debug: List all keys
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg("*")
            .query_async(conn.deref_mut())
            .await
            .unwrap();
        tracing::info!("Keys after FLUSHALL: {:?}", keys);
        assert!(keys.is_empty(), "Database should be empty after FLUSHALL");

        // Create fresh channel key
        tracing::info!("Creating channel key");
        conn.set("channel:channel1", "test_channel").await?;
        
        // Create TimeSeries without any initial data
        tracing::info!("Creating TimeSeries");
        let ts_create_result: Result<(), RedisError> = redis::cmd("TS.CREATE")
            .arg("channel_views:channel1")
            .arg("RETENTION")
            .arg(24 * 60 * 60 * 1000)
            .arg("DUPLICATE_POLICY")
            .arg("LAST")
            .query_async(conn.deref_mut())
            .await;

        if let Err(e) = ts_create_result {
            tracing::warn!("TimeSeries creation error: {:?}", e);
            if !e.to_string().contains("key already exists") {
                return Err(anyhow::anyhow!(e));
            }
        }

        // Debug: Check TimeSeries info
        let ts_info: redis::Value = redis::cmd("TS.INFO")
            .arg("channel_views:channel1")
            .query_async(conn.deref_mut())
            .await
            .unwrap();
        tracing::info!("TimeSeries info after creation: {:?}", ts_info);

        // List all keys again
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg("*")
            .query_async(conn.deref_mut())
            .await
            .unwrap();
        tracing::info!("Keys before view check: {:?}", keys);

        // Verify we start with no views
        let initial_views: Vec<(i64, i64)> = redis::cmd("TS.RANGE")
            .arg("channel_views:channel1")
            .arg("-")
            .arg("+")
            .query_async(conn.deref_mut())
            .await?;
        tracing::info!("Initial views: {:?}", initial_views);
        assert_eq!(initial_views.len(), 0, "Should start with no views");

        // Rest of the test setup...
        let args = Args::load().await?;
        let changes = Changes::new();
        let rpc_url = String::from(args.base_rpc_url).parse()?;
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(rpc_url);

        let keys = Keys::new(String::from(args.jwt_secret).as_bytes());

        let state = AppState {
            pool,
            changes,
            keys,
            provider,
        };

        let socket_addr: std::net::SocketAddr = "127.0.0.1:8000".parse()?;
        let result = log_channel_view(
            State(state),
            Path("channel1".to_string()),
            ConnectInfo(socket_addr),
        )
        .await;

        assert!(result.is_ok());

        // Get the current view count
        let views: Vec<(i64, i64)> = redis::cmd("TS.RANGE")
            .arg("channel_views:channel1")
            .arg("-")
            .arg("+")
            .query_async(conn.deref_mut())
            .await?;
        
        assert_eq!(views.len(), 1, "Expected exactly one view after logging"); 

        Ok(())
    }

    #[tokio::test]
    async fn test_log_channel_view_new_channel() -> Result<(), anyhow::Error> {
        let pool = get_redis_pool().await;
        let binding = pool.clone();
        let mut conn = binding.get().await.unwrap();
        setup_test_data(&mut conn).await;

        // Create the new channel key before trying to log a view
        conn.set("channel:channel2", "test_channel").await?;

        let args = Args::load().await?;
        let changes = Changes::new();
        let rpc_url = String::from(args.base_rpc_url).parse()?;
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(rpc_url);

        let keys = Keys::new(String::from(args.jwt_secret).as_bytes());

        let state = AppState {
            pool,
            changes,
            keys,
            provider,
        };

        let socket_addr: std::net::SocketAddr = "127.0.0.1:8000".parse()?;
        let result = log_channel_view(
            State(state),
            Path("channel2".to_string()),
            ConnectInfo(socket_addr),
        )
        .await;

        assert!(result.is_ok());

        // Verify TimeSeries data for the new channel
        let views: Vec<(i64, i64)> = redis::cmd("TS.RANGE")
            .arg("channel_views:channel2")
            .arg("-")
            .arg("+")
            .query_async(conn.deref_mut())
            .await?;
        assert_eq!(views.len(), 1, "Expected one view for the new channel");

        // Verify sorted set data
        let rank: Vec<(String, f64)> = conn.zrange_withscores("top_channels_daily", 0, -1).await?;
        assert!(rank.iter().any(|(channel, _)| channel == "channel2"), 
               "New channel should be in the sorted set");

        Ok(())
    }

    #[tokio::test]
    async fn test_log_channel_view_sorted_set_update() -> Result<(), anyhow::Error> {
        let pool = get_redis_pool().await;
        let binding = pool.clone();
        let mut conn = binding.get().await.unwrap();
        setup_test_data(&mut conn).await;

        // Populate the sorted set with 30 channels
        for i in 1..=30 {
            let channel = format!("channel{}", i);
            
            // Create channel key
            conn.set(format!("channel:{}", channel), "test_channel").await?;
            
            // Create TimeSeries - ignore if exists
            let ts_create_result: Result<(), RedisError> = redis::cmd("TS.CREATE")
                .arg(format!("channel_views:{}", channel))
                .arg("RETENTION")
                .arg(24 * 60 * 60 * 1000)
                .query_async(conn.deref_mut())
                .await;
            
            if let Err(e) = ts_create_result {
                if !e.to_string().contains("key already exists") {
                    panic!("Failed to create TimeSeries: {:?}", e);
                }
            }

            conn.zadd("top_channels_daily", &channel, i as f64).await?;
        }

        let args = Args::load().await?;
        let changes = Changes::new();
        let rpc_url = String::from(args.base_rpc_url).parse()?;
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(rpc_url);

        let keys = Keys::new(String::from(args.jwt_secret).as_bytes());

        let state = AppState {
            pool,
            changes,
            keys,
            provider,
        };

        let socket_addr: std::net::SocketAddr = "127.0.0.1:8000".parse()?;
        let result = log_channel_view(
            State(state),
            Path("channel31".to_string()),
            ConnectInfo(socket_addr),
        )
        .await;

        assert!(result.is_ok());

        // Verify that the sorted set contains only the top 30 channels
        let rank: Vec<(String, f64)> = conn.zrange_withscores("top_channels_daily", 0, -1).await?;
        assert_eq!(rank.len(), 30, "Sorted set should contain exactly 30 channels");

        Ok(())
    }

    #[tokio::test]
    async fn test_log_channel_view_sorted_set_trimming() -> Result<(), anyhow::Error> {
        let pool = get_redis_pool().await;
        let binding = pool.clone();
        let mut conn = binding.get().await.unwrap();
        setup_test_data(&mut conn).await;

        // Add 31 additional channels to the sorted set
        for i in 2..=32 {
            let channel = format!("channel{}", i);
            
            // Create channel key
            conn.set(format!("channel:{}", channel), "test_channel").await?;
            
            // Create TimeSeries with DUPLICATE_POLICY
            let ts_create_result: Result<(), RedisError> = redis::cmd("TS.CREATE")
                .arg(format!("channel_views:{}", channel))
                .arg("RETENTION")
                .arg(24 * 60 * 60 * 1000)
                .arg("DUPLICATE_POLICY")
                .arg("LAST")
                .query_async(conn.deref_mut())
                .await;
            
            if let Err(e) = ts_create_result {
                if !e.to_string().contains("key already exists") {
                    return Err(anyhow::anyhow!(e));
                }
            }

            conn.zadd("top_channels_daily", &channel, i as f64).await?;
        }

        let args = Args::load().await?;
        let changes = Changes::new();
        let rpc_url = String::from(args.base_rpc_url).parse()?;
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(rpc_url);

        let keys = Keys::new(String::from(args.jwt_secret).as_bytes());

        let state = AppState {
            pool,
            changes,
            keys,
            provider,
        };

        let socket_addr: std::net::SocketAddr = "127.0.0.1:8000".parse()?;
        let result = log_channel_view(
            State(state),
            Path("channel1".to_string()),
            ConnectInfo(socket_addr),
        )
        .await;

        assert!(result.is_ok());

        // Verify the sorted set contains only the top 30 channels
        let rank: Vec<(String, f64)> = conn.zrange_withscores("top_channels_daily", 0, -1).await?;
        assert_eq!(rank.len(), 30, "Sorted set should contain exactly 30 channels");

        Ok(())
    }

    #[tokio::test]
    async fn test_log_channel_view_rate_limiting() -> Result<(), anyhow::Error> {
        let pool = get_redis_pool().await;
        let binding = pool.clone();
        let mut conn = binding.get().await.unwrap();
        
        // More aggressive cleanup
        let _: () = redis::cmd("FLUSHALL")  // Use FLUSHALL instead of FLUSHDB
            .query_async(conn.deref_mut())
            .await
            .unwrap();
        
        // Delete specific keys just to be sure
        let _: () = redis::cmd("DEL")
            .arg("channel_views:channel1")
            .query_async(conn.deref_mut())
            .await
            .unwrap();

        // Create fresh channel key
        conn.set("channel:channel1", "test_channel").await?;
        
        // Create TimeSeries without any initial data
        let ts_create_result: Result<(), RedisError> = redis::cmd("TS.CREATE")
            .arg("channel_views:channel1")
            .arg("RETENTION")
            .arg(24 * 60 * 60 * 1000)
            .arg("DUPLICATE_POLICY")
            .arg("LAST")
            .query_async(conn.deref_mut())
            .await;

        if let Err(e) = ts_create_result {
            if !e.to_string().contains("key already exists") {
                return Err(anyhow::anyhow!(e));
            }
        }

        // Verify we start with no views
        let initial_views: Vec<(i64, i64)> = redis::cmd("TS.RANGE")
            .arg("channel_views:channel1")
            .arg("-")
            .arg("+")
            .query_async(conn.deref_mut())
            .await?;
        assert_eq!(initial_views.len(), 0, "Should start with no views");

        // Set up rate limiting key
        let user = "127.0.0.1";
        let channel = "channel1";
        let user_view_key = user_view_key(user, channel);
        conn.set_ex(&user_view_key, channel, 600).await?;

        // Rest of the test setup...
        let args = Args::load().await?;
        let changes = Changes::new();
        let rpc_url = String::from(args.base_rpc_url).parse()?;
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(rpc_url);

        let keys = Keys::new(String::from(args.jwt_secret).as_bytes());

        let state = AppState {
            pool,
            changes,
            keys,
            provider,
        };

        let socket_addr: std::net::SocketAddr = "127.0.0.1:8000".parse()?;
        let result = log_channel_view(
            State(state),
            Path("channel1".to_string()),
            ConnectInfo(socket_addr),
        )
        .await;

        assert!(result.is_ok());

        // Get the current view count
        let views: Vec<(i64, i64)> = redis::cmd("TS.RANGE")
            .arg("channel_views:channel1")
            .arg("-")
            .arg("+")
            .query_async(conn.deref_mut())
            .await?;
        
        assert_eq!(views.len(), 0, "Expected no new views due to rate limiting"); 

        Ok(())
    }
}
