use {
    crate::utils::keys::channel_view_key,
    anyhow::Result,
    bb8::PooledConnection,
    bb8_redis::RedisConnectionManager,
    std::ops::DerefMut,    
};

pub async fn get_channel_lifetime_views(
    conn: &mut PooledConnection<'_, RedisConnectionManager>,
    channel: &str,
) -> Result<i64> {
    let channel_view_key = channel_view_key(channel);

    // Use TS.GET to fetch the last value from the time series
    let result: Option<(i64, i64)> = redis::cmd("TS.GET")
        .arg(&channel_view_key)
        .query_async(conn.deref_mut())
        .await?;

    // Extract the value or return 0 if no data points are present
    Ok(result.map(|(_, value)| value).unwrap_or(0))
}