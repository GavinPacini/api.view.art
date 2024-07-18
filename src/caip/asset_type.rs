// asset_type = chain_id / asset_name
// chain_id = chain_namespace : chain_reference
// asset_name = asset_namespace : asset_reference
// Stargaze Token
// cosmos:stargaze-1/slip44:563

use {
    super::{
        asset_name::{AssetName, AssetNameError},
        chain::{ChainId, ChainIdError},
    },
    regex::Regex,
    serde::{Deserialize, Deserializer, Serialize, Serializer},
    std::{fmt::Display, str::FromStr},
};

struct Caip19AssetTypeSpec<'a> {
    asset_type_regex: &'a str,
}

const CAIP19_ASSET_TYPE_SPEC: Caip19AssetTypeSpec<'static> = Caip19AssetTypeSpec {
    asset_type_regex: r"[-:a-zA-Z0-9]{11,115}",
};

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum AssetTypeError {
    InvalidChainId(ChainIdError),
    InvalidAssetName(AssetNameError),
    InvalidType,
}

impl Display for AssetTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AssetTypeError::InvalidChainId(err) => write!(f, "invalid chain id: {}", err),
            AssetTypeError::InvalidAssetName(err) => write!(f, "invalid asset name: {}", err),
            AssetTypeError::InvalidType => write!(f, "invalid asset type"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Hash, Eq)]
pub struct AssetType {
    chain_id: ChainId,
    asset_name: AssetName,
}

impl FromStr for AssetType {
    type Err = AssetTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !Regex::new(CAIP19_ASSET_TYPE_SPEC.asset_type_regex)
            .unwrap()
            .is_match(s)
        {
            return Err(AssetTypeError::InvalidType);
        }
        let c: Vec<&str> = s.split('/').collect();
        if c.len() != 2 {
            return Err(AssetTypeError::InvalidType);
        }

        let chain_id = ChainId::from_str(c[0]).map_err(AssetTypeError::InvalidChainId)?;
        let asset_name = AssetName::from_str(c[1]).map_err(AssetTypeError::InvalidAssetName)?;

        Ok(AssetType {
            chain_id,
            asset_name,
        })
    }
}

impl Display for AssetType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.chain_id, self.asset_name)
    }
}

impl Serialize for AssetType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AssetType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        AssetType::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{AssetName, AssetType, ChainId, FromStr};

    #[test]
    fn valid_asset_type() {
        let asset_type = AssetType::from_str("cosmos:stargaze-1/slip44:563");
        assert_eq!(
            asset_type.unwrap(),
            AssetType {
                chain_id: ChainId::from_str("cosmos:stargaze-1").unwrap(),
                asset_name: AssetName::from_str("slip44:563").unwrap(),
            }
        )
    }
}
