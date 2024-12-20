use {
    crate::utils::secret_string::SecretString,
    anyhow::Result,
    axum::http::HeaderValue,
    clap::Parser,
};

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Args {
    /// Port to listen on
    #[arg(short, long, env, default_value = "3000")]
    pub port: u16,

    /// Redis connection URI
    #[arg(long, env)]
    pub rediscloud_url: SecretString,

    /// Base RPC URL
    #[arg(long, env)]
    pub base_rpc_url: SecretString,

    /// Secret key for JWT
    #[arg(long, env)]
    pub jwt_secret: SecretString,

    /// Privy App ID
    #[arg(long, env)]
    pub privy_app_id: SecretString,

    /// Privy App Secret
    #[arg(long, env)]
    pub privy_app_secret: SecretString,

    /// Privy Public Key
    #[arg(long, env)]
    pub privy_public_key: SecretString,

    /// Optional list of extra allowed origins
    #[arg(long, env, use_value_delimiter(true), value_delimiter(','))]
    pub allowed_origins: Option<Vec<HeaderValue>>,
}

impl Args {
    pub async fn load() -> Result<Self> {
        let _ = dotenvy::dotenv().map_err(|err| tracing::error!("dotenvy error: {err}"));
        Ok(Self::parse())
    }
}
