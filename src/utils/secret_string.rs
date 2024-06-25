use std::str::FromStr;

#[derive(Clone)]
pub struct SecretString(String);

impl From<SecretString> for String {
    fn from(val: SecretString) -> Self {
        val.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SecretString").field(&"********").finish()
    }
}

impl FromStr for SecretString {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(SecretString(s.to_string()))
    }
}
