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

    /// Redis TLS connection URI
    #[arg(long, env)]
    pub redis_tls_url: SecretString,
}

impl Args {
    pub async fn load() -> Result<Self> {
        let _ = dotenvy::dotenv().map_err(|err| tracing::error!("dotenvy error: {err}"));
        Ok(Self::parse())
    }
}
