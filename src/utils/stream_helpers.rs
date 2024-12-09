use bb8::PooledConnection;
use bb8_redis::RedisConnectionManager;
use anyhow::Result;
use std::ops::DerefMut;

pub async fn get_channel_lifetime_views(
    conn: &mut PooledConnection<'_, RedisConnectionManager>,
    channel: &str,
) -> Result<i64> {
    let page_view_key = format!("page_views:{}", channel);

    // Dereference the pooled connection to access the underlying Redis connection
    let redis_conn = conn.deref_mut();

    // Execute TS.RANGE command
    let data_points: Vec<(i64, i64)> = match redis::cmd("TS.RANGE")
        .arg(&page_view_key)
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

    // Sum all counts from data points
    let total_views: i64 = data_points.iter().map(|(_, count)| *count).sum();
    Ok(total_views)
}
