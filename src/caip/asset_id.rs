// asset_id = asset_type / token_id
// asset_type = chain_id / asset_name
// CryptoKitties NFT ID
// eip155:1/erc721:0x06012c8cf97BEaD5deAe237070F9587f8E7A266d/771769
// Stargaze NFT ID
// cosmos:stargaze-1/sg721:stars14x8psqx3xzhmqtdv4qw6v43uf5305sfe0nt4u4/771769

use {
    super::asset_type::{AssetType, AssetTypeError},
    regex::Regex,
    serde::{Deserialize, Deserializer, Serialize, Serializer},
    std::{fmt::Display, str::FromStr},
};

struct Caip19Spec<'a> {
    asset_id_regex: &'a str,
    token_id_regex: &'a str,
}

const CAIP19_SPEC: Caip19Spec<'static> = Caip19Spec {
    asset_id_regex: r"[-:a-zA-Z0-9]{13,148}",
    token_id_regex: r"[-a-zA-Z0-9]{1,32}",
};

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum AssetIdError {
    InvalidAssetType(AssetTypeError),
    InvalidAssetId,
    InvalidAssetIdNumComponents,
    InvalidTokenId,
}

impl Display for AssetIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AssetIdError::InvalidAssetType(err) => write!(f, "invalid asset type: {}", err),
            AssetIdError::InvalidAssetId => write!(f, "invalid asset id"),
            AssetIdError::InvalidAssetIdNumComponents => {
                write!(f, "invalid asset id number of components")
            }
            AssetIdError::InvalidTokenId => write!(f, "invalid asset id token id"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Hash, Eq)]
pub struct AssetId {
    asset_type: AssetType,
    token_id: String,
}

impl FromStr for AssetId {
    type Err = AssetIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !Regex::new(CAIP19_SPEC.asset_id_regex).unwrap().is_match(s) {
            return Err(AssetIdError::InvalidAssetId);
        }
        let c: Vec<&str> = s.rsplitn(2, '/').collect();
        if c.len() != 2 {
            return Err(AssetIdError::InvalidAssetIdNumComponents);
        }

        let asset_type = AssetType::from_str(c[1]).map_err(AssetIdError::InvalidAssetType)?;

        let token_id = c[0];
        if !Regex::new(CAIP19_SPEC.token_id_regex)
            .unwrap()
            .is_match(token_id)
        {
            return Err(AssetIdError::InvalidTokenId);
        }

        Ok(AssetId {
            asset_type,
            token_id: token_id.to_string(),
        })
    }
}

impl Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.asset_type, self.token_id)
    }
}

impl Serialize for AssetId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        AssetId::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{AssetId, AssetType, FromStr};

    #[test]
    fn valid_asset_id_eth() {
        let asset_id =
            AssetId::from_str("eip155:1/erc721:0x06012c8cf97BEaD5deAe237070F9587f8E7A266d/771769");
        assert_eq!(
            asset_id.unwrap(),
            AssetId {
                asset_type: AssetType::from_str(
                    "eip155:1/erc721:0x06012c8cf97BEaD5deAe237070F9587f8E7A266d"
                )
                .unwrap(),
                token_id: "771769".to_string()
            }
        )
    }

    #[test]
    fn valid_asset_id_stargaze() {
        let asset_id = AssetId::from_str(
            "cosmos:stargaze-1/sg721:stars14x8psqx3xzhmqtdv4qw6v43uf5305sfe0nt4u4/771769",
        );
        assert_eq!(
            asset_id.unwrap(),
            AssetId {
                asset_type: AssetType::from_str(
                    "cosmos:stargaze-1/sg721:stars14x8psqx3xzhmqtdv4qw6v43uf5305sfe0nt4u4"
                )
                .unwrap(),
                token_id: "771769".to_string()
            }
        )
    }
}
