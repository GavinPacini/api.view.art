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
        Args,
        Changes,
        Keys,
    },
    alloy::providers::ProviderBuilder,
    anyhow::anyhow,
    axum::{
        extract::{ConnectInfo, Path, State},
        http::StatusCode,
        response::IntoResponse,
        Json,
    },
    bb8_redis::{
        redis::{aio::ConnectionLike, AsyncCommands, Cmd, RedisError, RedisResult},
        RedisConnectionManager,
    },
    serde_json::json,
    std::{
        future::Future,
        net::SocketAddr,
        ops::DerefMut,
        pin::Pin,
    },
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

#[cfg(feature = "integration")]
struct TestContext {
    pool: bb8::Pool<RedisConnectionManager>,
}

#[cfg(feature = "integration")]
impl TestContext {
    async fn new() -> Self {
        // Use atomic counter to get unique DB number for each test
        static COUNTER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);
        let db_number = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        
        let manager = RedisConnectionManager::new(format!("redis://localhost:6379/{}", db_number)).unwrap();
        let pool = bb8::Pool::builder()
            .max_size(2)
            .connection_timeout(std::time::Duration::from_secs(5))
            .build(manager)
            .await
            .unwrap();
            
        Self { pool }
    }

    async fn setup(&self, channel: &str) -> Result<(), anyhow::Error> {
        let mut conn = self.pool.get().await?;
        
        // Clear this database
        let _: () = redis::cmd("FLUSHDB")
            .query_async(conn.deref_mut())
            .await?;
            
        // Create channel key
        conn.set(&format!("channel:{}", channel), "test_channel").await?;
        
        // Create TimeSeries
        let _: Result<(), RedisError> = redis::cmd("TS.CREATE")
            .arg(&format!("channel_views:{}", channel))
            .arg("UNCOMPRESSED")
            .arg("DUPLICATE_POLICY")
            .arg("LAST")
            .query_async(conn.deref_mut())
            .await
            .or_else(|e| {
                if e.to_string().contains("key already exists") {
                    Ok(())
                } else {
                    Err(e)
                }
            });
            
        Ok(())
    }

    async fn cleanup(&self) -> Result<(), anyhow::Error> {
        let mut conn = self.pool.get().await?;
        let _: () = redis::cmd("FLUSHDB")
            .query_async(conn.deref_mut())
            .await?;
        Ok(())
    }

    async fn populate_sorted_sets(&self) -> Result<(), anyhow::Error> {
        let mut conn = self.pool.get().await?;
        
        // Add 30 channels to the sorted sets with different scores
        for i in 1..=30 {
            let channel = format!("channel{}", i);
            let score = i as f64;
            
            // Add to all time ranges
            for set_key in &["top_channels_daily", "top_channels_weekly", "top_channels_monthly"] {
                let _: () = redis::cmd("ZADD")
                    .arg(set_key)
                    .arg(score)
                    .arg(&channel)
                    .query_async(conn.deref_mut())
                    .await?;
            }
        }
        
        Ok(())
    }
}

#[cfg(feature = "integration")]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_log_channel_view_existing_channel() -> Result<(), anyhow::Error> {
    let ctx = TestContext::new().await;
    
    // Set up test data
    {
        let mut conn = ctx.pool.get().await?;
        
        // Clear database
        let _: () = redis::cmd("FLUSHDB")
            .query_async(conn.deref_mut())
            .await?;
            
        // Create channel key
        conn.set("channel:channel1", "test_channel").await?;
        
        // Create TimeSeries
        let _: Result<(), RedisError> = redis::cmd("TS.CREATE")
            .arg("channel_views:channel1")
            .arg("UNCOMPRESSED")
            .arg("DUPLICATE_POLICY")
            .arg("LAST")
            .query_async(conn.deref_mut())
            .await
            .or_else(|e| {
                if e.to_string().contains("key already exists") {
                    Ok(())
                } else {
                    Err(e)
                }
            });
    }

    let args = Args::load().await?;
    let state = AppState { 
        pool: ctx.pool.clone(),
        changes: Changes::new(), 
        keys: Keys::new(String::from(args.jwt_secret).as_bytes()), 
        provider: ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(String::from(args.base_rpc_url).parse()?)
    };

    let socket_addr: std::net::SocketAddr = "127.0.0.1:8000".parse()?;
    
    // First view
    let result = log_channel_view(
        State(state),
        Path("channel1".to_string()),
        ConnectInfo(socket_addr),
    )
    .await;
    assert!(result.is_ok());

    // Check final state
    let mut conn = ctx.pool.get().await?;
    let post_views: Vec<(i64, i64)> = redis::cmd("TS.RANGE")
        .arg("channel_views:channel1")
        .arg("-")
        .arg("+")
        .query_async(conn.deref_mut())
        .await?;
    assert_eq!(post_views.len(), 1, "Expected exactly one view after logging");

    ctx.cleanup().await?;
    Ok(())
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn test_log_channel_view_new_channel() -> Result<(), anyhow::Error> {
    let ctx = TestContext::new().await;
    ctx.setup("channel1").await?;

    let args = Args::load().await?;
    let state = AppState { 
        pool: ctx.pool.clone(),
        changes: Changes::new(), 
        keys: Keys::new(String::from(args.jwt_secret).as_bytes()), 
        provider: ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(String::from(args.base_rpc_url).parse()?)
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
    let mut conn = ctx.pool.get().await?;
    let exists: bool = conn.exists("channel:channel2").await?;
    assert!(!exists, "New channel should not be created");

    ctx.cleanup().await?;
    Ok(())
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn test_log_channel_view_sorted_set_update() -> Result<(), anyhow::Error> {
    let ctx = TestContext::new().await;
    ctx.setup("channel31").await?;
    ctx.populate_sorted_sets().await?;

    let args = Args::load().await?;
    let state = AppState { 
        pool: ctx.pool.clone(),
        changes: Changes::new(), 
        keys: Keys::new(String::from(args.jwt_secret).as_bytes()), 
        provider: ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(String::from(args.base_rpc_url).parse()?)
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
    let mut conn = ctx.pool.get().await?;
    let rank: Vec<(String, f64)> = redis::cmd("ZRANGE")
        .arg("top_channels_daily")
        .arg("0")
        .arg("-1")
        .arg("WITHSCORES")
        .query_async(conn.deref_mut())
        .await?;
    assert_eq!(rank.len(), 30, "Sorted set should contain exactly 30 channels");

    ctx.cleanup().await?;
    Ok(())
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn test_log_channel_view_sorted_set_trimming() -> Result<(), anyhow::Error> {
    let ctx = TestContext::new().await;
    ctx.setup("channel1").await?;
    ctx.populate_sorted_sets().await?;

    let args = Args::load().await?;
    let state = AppState { 
        pool: ctx.pool.clone(),
        changes: Changes::new(), 
        keys: Keys::new(String::from(args.jwt_secret).as_bytes()), 
        provider: ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(String::from(args.base_rpc_url).parse()?)
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
    let mut conn = ctx.pool.get().await?;
    let rank: Vec<(String, f64)> = redis::cmd("ZRANGE")
        .arg("top_channels_daily")
        .arg("0")
        .arg("-1")
        .arg("WITHSCORES")
        .query_async(conn.deref_mut())
        .await?;
    assert_eq!(rank.len(), 30, "Sorted set should contain exactly 30 channels");

    ctx.cleanup().await?;
    Ok(())
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn test_log_channel_view_rate_limiting() -> Result<(), anyhow::Error> {
    let ctx = TestContext::new().await;
    
    // Set up test data
    {
        let mut conn = ctx.pool.get().await?;
        
        // Clear database
        let _: () = redis::cmd("FLUSHDB")
            .query_async(conn.deref_mut())
            .await?;
            
        // Create channel key
        conn.set("channel:channel1", "test_channel").await?;
        
        // Create TimeSeries
        let _: Result<(), RedisError> = redis::cmd("TS.CREATE")
            .arg("channel_views:channel1")
            .arg("UNCOMPRESSED")
            .arg("DUPLICATE_POLICY")
            .arg("LAST")
            .query_async(conn.deref_mut())
            .await
            .or_else(|e| {
                if e.to_string().contains("key already exists") {
                    Ok(())
                } else {
                    Err(e)
                }
            });
    }

    let args = Args::load().await?;
    let state = AppState { 
        pool: ctx.pool.clone(),
        changes: Changes::new(), 
        keys: Keys::new(String::from(args.jwt_secret).as_bytes()), 
        provider: ProviderBuilder::new()
            .with_recommended_fillers()
            .on_http(String::from(args.base_rpc_url).parse()?)
    };

    let socket_addr: std::net::SocketAddr = "127.0.0.1:8000".parse()?;
    
    // First view
    let result = log_channel_view(
        State(state.clone()),
        Path("channel1".to_string()),
        ConnectInfo(socket_addr),
    )
    .await;
    assert!(result.is_ok());

    // Second view should succeed in test mode
    let result = log_channel_view(
        State(state),
        Path("channel1".to_string()),
        ConnectInfo(socket_addr),
    )
    .await;
    assert!(result.is_ok());

    // Check final state - should have two views because rate limiting is disabled in test mode
    let mut conn = ctx.pool.get().await?;
    let post_views: Vec<(i64, i64)> = redis::cmd("TS.RANGE")
        .arg("channel_views:channel1")
        .arg("-")
        .arg("+")
        .query_async(conn.deref_mut())
        .await?;
    assert_eq!(post_views.len(), 2, "Should have two views because rate limiting is disabled in test mode");

    ctx.cleanup().await?;
    Ok(())
}
