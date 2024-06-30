use {
    crate::args::Args,
    anyhow::{Context, Result},
    axum::{
        extract::{Path, State},
        http::{header, HeaderValue, Method, StatusCode},
        response::{
            sse::{Event, Sse},
            IntoResponse,
        },
        routing::get,
        Json,
        Router,
    },
    bb8::Pool,
    bb8_redis::{redis::AsyncCommands, RedisConnectionManager},
    changes::Changes,
    futures::{stream::Stream, StreamExt},
    model::PlaylistData,
    serde_json::json,
    std::{
        convert::Infallible,
        net::{Ipv4Addr, SocketAddr},
        time::Duration,
    },
    tokio_stream::wrappers::BroadcastStream,
    tower_http::{cors::CorsLayer, trace::TraceLayer},
    tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt},
};

mod args;
mod changes;
mod model;
mod utils;

#[derive(Clone)]
struct AppState {
    pool: Pool<RedisConnectionManager>,
    changes: Changes,
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

    let changes = Changes::new();

    // build our application
    let app = app(AppState { pool, changes });

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
    // build our application with a route
    Router::new()
        .route(
            "/v1/playlist/:channel",
            get(get_playlist).post(set_playlist),
        )
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::new()
                .allow_origin([
                    "http://localhost:5173".parse::<HeaderValue>().unwrap(),
                    "https://view.art".parse::<HeaderValue>().unwrap(),
                ])
                .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
                .allow_headers([header::ACCEPT, header::CONTENT_TYPE]),
        )
        .with_state(state)
}

async fn get_playlist(
    state: State<AppState>,
    Path(channel): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    tracing::info!("get playlist for channel {}", channel);

    let initial_playlist = {
        match state.pool.get().await {
            Ok(mut conn) => match conn.get(&channel).await {
                Ok(playlist) => playlist,
                Err(err) => {
                    tracing::error!("Error getting playlist for channel {}: {:?}", channel, err);
                    PlaylistData {
                        playlist: 0,
                        offset: 0,
                    }
                }
            },
            Err(err) => {
                tracing::error!("Error getting connection from pool: {:?}", err);
                PlaylistData {
                    playlist: 0,
                    offset: 0,
                }
            }
        }
    };

    let (tx, rx) = {
        let mut changes = state.changes.clone();
        changes.subscribe(channel.clone()).await
    };

    let stream = BroadcastStream::new(rx).map(|playlist| {
        let event = match playlist {
            Ok(playlist) => Event::default()
                .json_data(playlist)
                .unwrap()
                .event("playlist"),
            Err(err) => {
                tracing::error!("Error getting playlist: {:?}", err);
                Event::default()
            }
        };

        Ok::<Event, Infallible>(event)
    });

    match tx.send(initial_playlist) {
        Ok(len) => {
            tracing::debug!("sent {} to {} receivers", channel, len);
        }
        Err(err) => {
            tracing::error!("Error sending initial playlist: {:?}", err);
        }
    }

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(1))
            .text("keep-alive-text"),
    )
}

async fn set_playlist(
    state: State<AppState>,
    Path(channel): Path<String>,
    Json(playlist): Json<PlaylistData>,
) -> impl IntoResponse {
    tracing::info!("set playlist for channel {}", channel);

    match state.pool.get().await {
        Ok(mut conn) => match conn
            .set::<&str, String, ()>(&channel, serde_json::to_string(&playlist).unwrap())
            .await
        {
            Ok(_) => {
                state.changes.broadcast(&channel, playlist).await;
                (StatusCode::OK, json!({ "status": true }).to_string())
            }
            Err(err) => {
                tracing::error!("Error setting playlist for channel {}: {:?}", channel, err);
                internal_error(err)
            }
        },
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            internal_error(err)
        }
    }
}

/// Utility function for mapping any error into a `500 Internal Server Error`
/// response.
fn internal_error<E>(err: E) -> (StatusCode, String)
where
    E: std::error::Error,
{
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({ "status": false, "error": err.to_string() }).to_string(),
    )
}

#[cfg(test)]
mod tests {
    use {super::*, eventsource_stream::Eventsource, tokio::net::TcpListener};

    const REDIS_URL: &str = "redis://localhost:6379";

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

        let changes = Changes::new();

        tokio::spawn(async {
            axum::serve(listener, app(AppState { pool, changes }))
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

                            // set playlist to 1
                            reqwest::Client::new()
                                .post(&format!("{}/v1/playlist/test", listening_url))
                                .header("User-Agent", "integration_test")
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
    }
}
