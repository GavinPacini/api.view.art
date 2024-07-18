// asset_name = asset_namespace : asset_reference

use {
    regex::Regex,
    serde::{Deserialize, Deserializer, Serialize, Serializer},
    std::{fmt::Display, str::FromStr},
};

struct Caip19AssetNameSpec<'a> {
    asset_name_regex: &'a str,
    asset_namespace_regex: &'a str,
    asset_reference_regex: &'a str,
}

const CAIP19_ASSET_NAME_SPEC: Caip19AssetNameSpec<'static> = Caip19AssetNameSpec {
    asset_name_regex: r"[-:a-zA-Z0-9]{5,73}",
    asset_namespace_regex: r"[-a-z0-9]{3,8}",
    asset_reference_regex: r"[-a-zA-Z0-9]{1,64}",
};

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum AssetNameError {
    InvalidName,
    InvalidNamespace,
    InvalidReference,
}

impl Display for AssetNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AssetNameError::InvalidName => write!(f, "invalid asset name"),
            AssetNameError::InvalidNamespace => write!(f, "invalid asset name namespace"),
            AssetNameError::InvalidReference => write!(f, "invalid asset name reference"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Hash, Eq)]
pub struct AssetName {
    namespace: String,
    reference: String,
}

impl FromStr for AssetName {
    type Err = AssetNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !Regex::new(CAIP19_ASSET_NAME_SPEC.asset_name_regex)
            .unwrap()
            .is_match(s)
        {
            return Err(AssetNameError::InvalidName);
        }
        let c: Vec<&str> = s.split(':').collect();
        if c.len() != 2 {
            return Err(AssetNameError::InvalidName);
        }
        let namespace = c[0];
        if !Regex::new(CAIP19_ASSET_NAME_SPEC.asset_namespace_regex)
            .unwrap()
            .is_match(namespace)
        {
            return Err(AssetNameError::InvalidNamespace);
        }
        let reference = c[1];
        if !Regex::new(CAIP19_ASSET_NAME_SPEC.asset_reference_regex)
            .unwrap()
            .is_match(reference)
        {
            return Err(AssetNameError::InvalidReference);
        }

        Ok(AssetName {
            namespace: namespace.to_string(),
            reference: reference.to_string(),
        })
    }
}

impl Display for AssetName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.namespace, self.reference)
    }
}

impl Serialize for AssetName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AssetName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        AssetName::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{AssetName, FromStr};

    #[test]
    fn valid_asset_name() {
        let name = AssetName::from_str("slip44:563");
        assert_eq!(
            name.unwrap(),
            AssetName {
                namespace: "slip44".to_string(),
                reference: "563".to_string(),
            }
        )
    }
}
