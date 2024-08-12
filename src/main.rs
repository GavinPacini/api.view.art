use {
    crate::args::Args,
    anyhow::{Context, Result},
    axum::{
        self,
        http::{header, HeaderValue, Method},
        routing::{get, post},
        Extension, Router,
    },
    bb8::Pool,
    bb8_redis::{redis::AsyncCommands, RedisConnectionManager},
    changes::Changes,
    ethers::providers::{Http, Middleware, Provider},
    routes::auth::Keys,
    std::net::{Ipv4Addr, SocketAddr},
    tower_http::{cors::CorsLayer, trace::TraceLayer},
    tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt},
};

mod args;
mod caip;
mod changes;
mod model;
mod routes;
mod utils;

#[derive(Clone)]
struct AppState {
    pool: Pool<RedisConnectionManager>,
    changes: Changes,
    provider: Provider<Http>,
    keys: Keys,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "api_view_art=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::load().await?;
    tracing::info!("Running with args: {args:?}");

    let manager = RedisConnectionManager::new::<String>(args.redis_url.into()).unwrap();
    let pool = Pool::builder().build(manager).await.unwrap();
    {
        // ping the database before starting
        let mut conn = pool.get().await.unwrap();
        conn.set::<&str, &str, ()>("foo", "bar").await.unwrap();
        let result: String = conn.get("foo").await.unwrap();
        assert_eq!(result, "bar");
    }
    tracing::debug!("successfully connected to redis and pinged it");

    let provider = Provider::<Http>::try_from(String::from(args.base_rpc_url)).unwrap();
    let chain_id = provider.get_chainid().await.unwrap();
    tracing::debug!("connected to chain id {:?}", chain_id);

    let keys = Keys::new(String::from(args.jwt_secret).as_bytes());

    let changes = Changes::new();

    // build our application
    let app = app(AppState {
        pool,
        changes,
        provider,
        keys,
    });

    // run it
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, args.port)))
            .await
            .context("Failed to bind to port")?;
    tracing::debug!("listening on {}", listener.local_addr()?);
    axum::serve(listener, app)
        .await
        .context("Failed to serve")?;

    Ok(())
}

fn app(state: AppState) -> Router {
    let keys = state.keys.clone();
    // build our application with a route
    Router::new().nest(
        "/v1",
        Router::new()
            .route("/nonce", post(routes::auth::get_nonce))
            .route("/auth", post(routes::auth::verify_auth))
            .route(
                "/channel/:channel",
                get(routes::channel::get_channel).post(routes::channel::set_channel),
            )
            .route("/channel/:channel/taken", get(routes::channel::is_taken))
            .route(
                "/channel/:channel/summary",
                get(routes::channel::get_summary),
            )
            .route(
                "/wallet/:address/channels",
                get(routes::wallet::get_channels),
            )
            .layer(TraceLayer::new_for_http())
            .layer(Extension(keys))
            .layer(
                CorsLayer::new()
                    .allow_origin([
                        "http://localhost:5173".parse::<HeaderValue>().unwrap(),
                        "https://view.art".parse::<HeaderValue>().unwrap(),
                    ])
                    .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
                    .allow_headers([header::ACCEPT, header::CONTENT_TYPE, header::AUTHORIZATION]),
            )
            .with_state(state),
    )
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        caip::asset_id::AssetId,
        chrono::Utc,
        eventsource_stream::Eventsource,
        futures::StreamExt,
        model::{ChannelContent, EmptyChannelContent, Item, Played},
        routes::auth::tests::get_team_api_key,
        serde_json::Value,
        tokio::net::TcpListener,
        url::Url,
    };

    const REDIS_URL: &str = "redis://localhost:6379";
    const BASE_RPC_URL: &str = "https://base.drpc.org";
    const JWT_SECRET: &str = "secret";

    /// A helper function that spawns our application in the background
    async fn spawn_app(host: impl Into<String>) -> String {
        let host = host.into();

        let listener = TcpListener::bind(format!("{}:0", host)).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let manager = RedisConnectionManager::new(REDIS_URL).unwrap();
        let pool = Pool::builder().build(manager).await.unwrap();
        {
            // flush redis before starting
            let mut conn = pool.get().await.unwrap();
            let _: () = bb8_redis::redis::cmd("FLUSHALL")
                .query_async(&mut *conn)
                .await
                .unwrap();
        }
        tracing::debug!("successfully connected to redis and pinged it");

        let provider = Provider::<Http>::try_from(BASE_RPC_URL).unwrap();
        let chain_id = provider.get_chainid().await.unwrap();
        tracing::debug!("connected to chain id {:?}", chain_id);

        let changes = Changes::new();

        tokio::spawn(async {
            axum::serve(
                listener,
                app(AppState {
                    pool,
                    changes,
                    provider,
                    keys: Keys::new(String::from(JWT_SECRET).as_bytes()),
                }),
            )
            .await
            .unwrap();
        });
        // Returns address (e.g. http://127.0.0.1{random_port})
        format!("http://{}:{}", host, port)
    }

    #[tokio::test]
    async fn integration_test() {
        let listening_url = spawn_app("127.0.0.1").await;

        let mut event_stream = reqwest::Client::new()
            .get(&format!("{}/v1/channel/test", listening_url))
            .header("User-Agent", "integration_test")
            .send()
            .await
            .unwrap()
            .bytes_stream()
            .eventsource()
            .take(3);

        let authorization = get_team_api_key(JWT_SECRET.to_string());

        let mut i = 0;
        while let Some(event) = event_stream.next().await {
            match event {
                Ok(event) => {
                    assert!(event.event == "content");

                    match i {
                        0 => {
                            let content =
                                serde_json::from_str::<EmptyChannelContent>(&event.data).unwrap();
                            assert!(content.empty);

                            // check if channel is taken
                            let result = reqwest::Client::new()
                                .get(&format!("{}/v1/channel/test/taken", listening_url))
                                .header("User-Agent", "integration_test")
                                .send()
                                .await
                                .unwrap();
                            assert!(result.status().is_success());
                            let content =
                                serde_json::from_str::<Value>(&result.text().await.unwrap())
                                    .unwrap();
                            assert!(!content["taken"].as_bool().unwrap());

                            // setting the channel without the team API key should fail
                            let result = reqwest::Client::new()
                                .post(&format!("{}/v1/channel/test", listening_url))
                                .header("User-Agent", "integration_test")
                                .json(&ChannelContent {
                                    items: vec![Item {
                                        id: "eip155:1/erc721:0x06012c8cf97BEaD5deAe237070F9587f8E7A266d/771769".parse::<AssetId>().unwrap(),
                                        title: "test".to_string(),
                                        artist: Some("test".to_string()),
                                        url: Url::parse("https://test.com").unwrap(),
                                        thumbnail_url: Url::parse("https://test.com").unwrap(),
                                        apply_matte: false,
                                        activate_by: "".to_string(),
                                    }],
                                    played: Played {
                                        item: 0,
                                        at: Utc::now(),
                                    },
                                })
                                .send()
                                .await
                                .unwrap();
                            assert!(result.status().is_client_error());

                            // set channel to 1 using the team API key
                            reqwest::Client::new()
                                .post(&format!("{}/v1/channel/test", listening_url))
                                .header("User-Agent", "integration_test")
                                .header("Authorization", format!("Bearer {}", authorization))
                                .json(&ChannelContent {
                                    items: vec![Item {
                                        id: "eip155:1/erc721:0x06012c8cf97BEaD5deAe237070F9587f8E7A266d/771769".parse::<AssetId>().unwrap(),
                                        title: "test".to_string(),
                                        artist: Some("test".to_string()),
                                        url: Url::parse("https://test.com").unwrap(),
                                        thumbnail_url: Url::parse("https://test.com").unwrap(),
                                        apply_matte: false,
                                        activate_by: "".to_string(),
                                    }],
                                    played: Played {
                                        item: 0,
                                        at: Utc::now(),
                                    },
                                })
                                .send()
                                .await
                                .unwrap();
                        }
                        1 => {
                            let content =
                                serde_json::from_str::<ChannelContent>(&event.data).unwrap();
                            assert!(content.items.len() == 1);
                            assert!(content.items[0].id == "eip155:1/erc721:0x06012c8cf97BEaD5deAe237070F9587f8E7A266d/771769".parse::<AssetId>().unwrap());
                            assert!(content.played.item == 0);

                            // check if channel is taken
                            let result = reqwest::Client::new()
                                .get(&format!("{}/v1/channel/test/taken", listening_url))
                                .header("User-Agent", "integration_test")
                                .send()
                                .await
                                .unwrap();
                            assert!(result.status().is_success());
                            let content =
                                serde_json::from_str::<Value>(&result.text().await.unwrap())
                                    .unwrap();
                            assert!(content["taken"].as_bool().unwrap());

                            // check if channel is in list of channels for address
                            let result = reqwest::Client::new()
                                .get(&format!(
                                    "{}/v1/wallet/{:#?}/channels",
                                    listening_url,
                                    ethers::types::Address::zero()
                                ))
                                .header("User-Agent", "integration_test")
                                .send()
                                .await
                                .unwrap();

                            assert!(result.status().is_success());
                            let content =
                                serde_json::from_str::<Value>(&result.text().await.unwrap())
                                    .unwrap();
                            assert!(content["channels"].as_array().unwrap().len() == 1);
                            assert!(content["channels"][0].as_str().unwrap() == "test");

                            // set played item to 1
                            reqwest::Client::new()
                                .post(&format!("{}/v1/channel/test", listening_url))
                                .header("User-Agent", "integration_test")
                                .header("Authorization", format!("Bearer {}", authorization))
                                .json(&ChannelContent {
                                    items: vec![Item {
                                        id: "eip155:1/erc721:0x06012c8cf97BEaD5deAe237070F9587f8E7A266d/771769".parse::<AssetId>().unwrap(),
                                        title: "test".to_string(),
                                        artist: Some("test".to_string()),
                                        url: Url::parse("https://test.com").unwrap(),
                                        thumbnail_url: Url::parse("https://test.com").unwrap(),
                                        apply_matte: false,
                                        activate_by: "".to_string(),
                                    }],
                                    played: Played {
                                        item: 1,
                                        at: Utc::now(),
                                    },
                                })
                                .send()
                                .await
                                .unwrap();
                        }
                        2 => {
                            let content =
                                serde_json::from_str::<ChannelContent>(&event.data).unwrap();
                            assert!(content.items.len() == 1);
                            assert!(content.items[0].id == "eip155:1/erc721:0x06012c8cf97BEaD5deAe237070F9587f8E7A266d/771769".parse::<AssetId>().unwrap());
                            assert!(content.played.item == 1);
                        }
                        _ => {
                            panic!("Unexpected event");
                        }
                    }

                    i += 1;
                }
                Err(_) => {
                    panic!("Error in event stream");
                }
            }
        }

        assert!(i == 3);

        // TODO: create a wallet
        // TODO: get nonce
        // TODO: generate and sign SIWE message
        // TODO: test we can set the channel channel correctly
    }
}
