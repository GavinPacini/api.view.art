use {
    bb8_redis::redis::{FromRedisValue, RedisError, RedisResult, Value},
    serde::{Deserialize, Serialize},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistData {
    pub playlist: u32,
    pub offset: u32,
}

impl FromRedisValue for PlaylistData {
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
