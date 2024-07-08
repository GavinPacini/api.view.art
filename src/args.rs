use {crate::utils::secret_string::SecretString, anyhow::Result, clap::Parser};

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Args {
    /// Port to listen on
    #[arg(short, long, env, default_value = "3000")]
    pub port: u16,

    /// Redis connection URI
    #[arg(long, env)]
    pub redis_url: SecretString,

    /// Base RPC URL
    #[arg(long, env)]
    pub base_rpc_url: SecretString,

    /// Secret key for JWT
    #[arg(long, env)]
    pub jwt_secret: SecretString,
}

impl Args {
    pub async fn load() -> Result<Self> {
        let _ = dotenvy::dotenv().map_err(|err| tracing::error!("dotenvy error: {err}"));
        Ok(Self::parse())
    }
}
