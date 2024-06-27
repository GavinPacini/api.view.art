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
    serde::Deserialize,
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
                .allow_origin("http://localhost:5173".parse::<HeaderValue>().unwrap())
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
                    0
                }
            },
            Err(err) => {
                tracing::error!("Error getting connection from pool: {:?}", err);
                0
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
                .data(json!({ "playlist": playlist }).to_string())
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

#[derive(Debug, Deserialize)]
struct SetPlaylist {
    playlist: u32,
}

async fn set_playlist(
    state: State<AppState>,
    Path(channel): Path<String>,
    Json(SetPlaylist { playlist }): Json<SetPlaylist>,
) -> impl IntoResponse {
    tracing::info!("set playlist for channel {}", channel);

    match state.pool.get().await {
        Ok(mut conn) => match conn.set::<&str, u32, ()>(&channel, playlist).await {
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

// #[cfg(test)]
// mod tests {
//     use {super::*, eventsource_stream::Eventsource, tokio::net::TcpListener};

//     #[tokio::test]
//     async fn integration_test() {
//         // A helper function that spawns our application in the background
//         async fn spawn_app(host: impl Into<String>) -> String {
//             let host = host.into();
//             // Bind to localhost at the port 0, which will let the OS assign
// an available             // port to us
//             let listener = TcpListener::bind(format!("{}:0",
// host)).await.unwrap();             // Retrieve the port assigned to us by the
// OS             let port = listener.local_addr().unwrap().port();

//             let db_client =
// redis::Client::open("redis://localhost:6379").unwrap();

//             let con =
// db_client.get_multiplexed_async_connection().await.unwrap();

//             tokio::spawn(async {
//                 axum::serve(listener, app(AppState { db: con }))
//                     .await
//                     .unwrap();
//             });
//             // Returns address (e.g. http://127.0.0.1{random_port})
//             format!("http://{}:{}", host, port)
//         }
//         let listening_url = spawn_app("127.0.0.1").await;

//         let mut event_stream = reqwest::Client::new()
//             .get(&format!("{}/v1/playlist/test", listening_url))
//             .header("User-Agent", "integration_test")
//             .send()
//             .await
//             .unwrap()
//             .bytes_stream()
//             .eventsource()
//             .take(1);

//         let mut event_data: Vec<String> = vec![];
//         while let Some(event) = event_stream.next().await {
//             match event {
//                 Ok(event) => {
//                     // break the loop at the end of SSE stream
//                     if event.data == "[DONE]" {
//                         break;
//                     }

//                     event_data.push(event.data);
//                 }
//                 Err(_) => {
//                     panic!("Error in event stream");
//                 }
//             }
//         }

//         assert!(event_data[0] == "hi!");
//     }
// }
