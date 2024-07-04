use {
    crate::args::Args,
    anyhow::{Context, Result},
    axum::{
        self,
        http::{header, HeaderValue, Method},
        routing::{get, post},
        Extension,
        Router,
    },
    bb8::Pool,
    bb8_redis::{redis::AsyncCommands, RedisConnectionManager},
    changes::Changes,
    ethers::providers::{Http, Middleware, Provider},
    model::PlaylistData,
    routes::auth::Keys,
    std::net::{Ipv4Addr, SocketAddr},
    tower_http::{cors::CorsLayer, trace::TraceLayer},
    tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt},
};

mod args;
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
    Router::new()
        .route("/v1/nonce", post(routes::auth::get_nonce))
        .route("/v1/auth", post(routes::auth::verify_auth))
        .route(
            "/v1/playlist/:channel",
            get(routes::playlist::get_playlist).post(routes::playlist::set_playlist),
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
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        eventsource_stream::Eventsource,
        futures::StreamExt,
        routes::auth::tests::get_team_api_key,
        tokio::net::TcpListener,
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
            .get(&format!("{}/v1/playlist/test", listening_url))
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
                    assert!(event.event == "playlist");

                    match i {
                        0 => {
                            let playlist =
                                serde_json::from_str::<PlaylistData>(&event.data).unwrap();
                            assert!(playlist.playlist == 0);
                            assert!(playlist.offset == 0);

                            // setting the playlist without the team API key should fail
                            let result = reqwest::Client::new()
                                .post(&format!("{}/v1/playlist/test", listening_url))
                                .header("User-Agent", "integration_test")
                                .json(&PlaylistData {
                                    playlist: 1,
                                    offset: 0,
                                })
                                .send()
                                .await
                                .unwrap();
                            assert!(result.status().is_client_error());

                            // set playlist to 1 using the team API key
                            reqwest::Client::new()
                                .post(&format!("{}/v1/playlist/test", listening_url))
                                .header("User-Agent", "integration_test")
                                .header("Authorization", format!("Bearer {}", authorization))
                                .json(&PlaylistData {
                                    playlist: 1,
                                    offset: 0,
                                })
                                .send()
                                .await
                                .unwrap();
                        }
                        1 => {
                            let playlist =
                                serde_json::from_str::<PlaylistData>(&event.data).unwrap();
                            assert!(playlist.playlist == 1);
                            assert!(playlist.offset == 0);

                            // set offset to 1
                            reqwest::Client::new()
                                .post(&format!("{}/v1/playlist/test", listening_url))
                                .header("User-Agent", "integration_test")
                                .header("Authorization", format!("Bearer {}", authorization))
                                .json(&PlaylistData {
                                    playlist: 1,
                                    offset: 1,
                                })
                                .send()
                                .await
                                .unwrap();
                        }
                        2 => {
                            let playlist =
                                serde_json::from_str::<PlaylistData>(&event.data).unwrap();
                            assert!(playlist.playlist == 1);
                            assert!(playlist.offset == 1);
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
        // TODO: test we can set the channel playlist correctly
    }
}
