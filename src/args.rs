use {anyhow::Result, clap::Parser};

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Args {
    /// Port to listen on
    #[arg(short, long, env, default_value = "3000")]
    pub port: u16,

    /// Redis connection URI
    #[arg(long, env)]
    pub redis: String,
}

impl Args {
    pub async fn load() -> Result<Self> {
        dotenvy::dotenv()?;
        Ok(Self::parse())
    }
}
