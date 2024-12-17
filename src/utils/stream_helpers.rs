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

    // Dereference the pooled connection to access the underlying Redis connection
    let redis_conn = conn.deref_mut();

    // Execute TS.RANGE command
    let data_points: Vec<(i64, i64)> = match redis::cmd("TS.RANGE")
        .arg(&channel_view_key)
        .arg("-")
        .arg("+")
        .query_async(redis_conn)
        .await
    {
        Ok(data) => data,
        Err(err) => {
            tracing::error!(
                "Error fetching view count for channel {}: {:?}",
                channel,
                err
            );
            return Err(anyhow::anyhow!("Failed to fetch view count: {:?}", err));
        }
    };

    Ok(data_points.len() as i64)
}