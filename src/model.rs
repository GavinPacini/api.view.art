use {
    crate::caip::asset_id::AssetId,
    alloy::primitives::{Address, Bytes},
    bb8_redis::redis::{FromRedisValue, RedisError, RedisResult, Value},
    chrono::{DateTime, Utc},
    serde::{Deserialize, Serialize},
    siwe::Message,
    url::Url,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAuth {
    pub address: Address,
    pub chain_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyAuth {
    pub message: Message,
    pub signature: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: AssetId,
    pub title: String,
    pub artist: Option<String>,
    pub url: Url,
    pub thumbnail_url: Url,
    pub apply_matte: bool,
    pub activate_by: String,
    pub predominant_color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub item: u32,
    pub at: DateTime<Utc>,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelContent {
    pub items: Vec<Item>,
    pub status: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmptyChannelContent {
    pub empty: bool,
}

impl Default for EmptyChannelContent {
    fn default() -> Self {
        Self { empty: true }
    }
}

impl FromRedisValue for ChannelContent {
    fn from_redis_value(v: &Value) -> RedisResult<Self> {
        let s = match v {
            Value::BulkString(data) => std::str::from_utf8(data).map_err(|err| {
                RedisError::from((
                    bb8_redis::redis::ErrorKind::TypeError,
                    "Error parsing string from utf8",
                    err.to_string(),
                ))
            })?,
            _ => {
                return Err(RedisError::from((
                    bb8_redis::redis::ErrorKind::TypeError,
                    "Unexpected Redis value type",
                )))
            }
        };
        serde_json::from_str(s).map_err(|err| {
            RedisError::from((
                bb8_redis::redis::ErrorKind::TypeError,
                "Error parsing Redis value",
                err.to_string(),
            ))
        })
    }
}
