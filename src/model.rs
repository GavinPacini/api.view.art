use {
    bb8_redis::redis::{FromRedisValue, RedisError, RedisResult, Value},
    chrono::{DateTime, Utc},
    ethers::types::Address,
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
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub url: Url,
    pub thumbnail_url: Url,
    pub apply_matte: bool,
    pub activate_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Played {
    pub item: u32,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelContent {
    // TODO: Should migrate to CDN for images, for now just a blob
    pub image: String,
    pub title: String,
    pub artists: String,
    pub items: Vec<Item>,
    pub played: Played,
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
            Value::Data(data) => std::str::from_utf8(data).unwrap(),
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
