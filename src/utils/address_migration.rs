use {
    super::keys::{address_key, old_address_key},
    alloy::primitives::Address,
    anyhow::{anyhow, Result},
    bb8::{PooledConnection, RunError},
    bb8_redis::RedisConnectionManager,
    redis::{AsyncCommands, RedisError},
};

/// Migrates piecemeal the address key from the old format to the new format
/// Merges any existing channels for the address
pub async fn migrate_addresses<'a>(
    address: &'a Address,
    conn: Result<PooledConnection<'a, RedisConnectionManager>, RunError<RedisError>>,
) -> Result<()> {
    let old_key = old_address_key(address);
    let new_key = address_key(address);

    match conn {
        Ok(mut conn) => match conn.exists(&old_key).await {
            Ok(exists) => {
                if exists {
                    tracing::info!("migrating address key from {} to {}", old_key, new_key);
                    match conn
                        .sunionstore::<&str, &[&str], ()>(&new_key, &[&old_key, &new_key])
                        .await
                    {
                        Ok(_) => {
                            match conn.del::<&str, ()>(&old_key).await {
                                Ok(_) => {
                                    tracing::info!("deleted old address key {}", old_key);
                                }
                                Err(err) => {
                                    tracing::error!("Error deleting old address key: {:?}", err);
                                }
                            }
                            tracing::info!("migrated address key from {} to {}", old_key, new_key);
                            Ok(())
                        }
                        Err(err) => {
                            tracing::error!("Error migrating address key: {:?}", err);
                            Err(anyhow!(err))
                        }
                    }
                } else {
                    Ok(())
                }
            }
            Err(err) => {
                tracing::error!("Error checking if address key exists: {:?}", err);
                Err(anyhow!(err))
            }
        },
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            Err(anyhow!(err))
        }
    }
}
