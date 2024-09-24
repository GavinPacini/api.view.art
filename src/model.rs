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
#[serde(untagged)]
#[serde(rename_all_fields = "camelCase")]
pub enum ChannelContent {
    ChannelContentV1 {
        items: Vec<Item>,
        played: Played,
    },
    ChannelContentV2 {
        items: Vec<Item>,
        #[serde(default = "default_item_duration")]
        item_duration: u32,
        status: Status,
    },
}

impl ChannelContent {
    pub fn items(&self) -> &Vec<Item> {
        match self {
            ChannelContent::ChannelContentV1 { items, .. } => items,
            ChannelContent::ChannelContentV2 { items, .. } => items,
        }
    }

    pub fn v2(self) -> ChannelContent {
        match self {
            ChannelContent::ChannelContentV1 { items, played } => {
                ChannelContent::ChannelContentV2 {
                    items,
                    item_duration: default_item_duration(),
                    status: Status {
                        item: played.item,
                        at: played.at,
                        action: Action::Played,
                    },
                }
            }
            content @ ChannelContent::ChannelContentV2 { .. } => content,
        }
    }
}

// ChannelContentV2

// Function to provide the default value
fn default_item_duration() -> u32 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub item: u32,
    pub at: DateTime<Utc>,
    pub action: Action,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Played,
    Paused,
}

// ChannelContentV1

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Played {
    pub item: u32,
    pub at: DateTime<Utc>,
}

// EmptyChannelContent

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
