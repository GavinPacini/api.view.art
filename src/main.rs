use {
    crate::args::Args,
    anyhow::{Context, Result},
    axum::{
        extract::{Path, State},
        response::{
            sse::{Event, Sse},
            IntoResponse,
        },
        routing::get,
        Router,
    },
    axum_extra::{headers, TypedHeader},
    futures::stream::{self, Stream},
    redis::aio::MultiplexedConnection,
    std::{
        convert::Infallible,
        net::{Ipv4Addr, SocketAddr},
        time::Duration,
    },
    tokio_stream::StreamExt as _,
    tower_http::trace::TraceLayer,
    tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt},
};

mod args;
mod utils;

#[derive(Clone)]
struct AppState {
    db: MultiplexedConnection,
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

    let db_client = redis::Client::open::<String>(args.redis_url.into())
        .context("Failed to connect to redis")?;

    let con = db_client
        .get_multiplexed_async_connection()
        .await
        .context("Failed to connect to Redis")?;

    // build our application
    let app = app(AppState { db: con });

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
            "/v1/playlist/:playlist",
            get(get_playlist).post(set_playlist),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn get_playlist(
    state: State<AppState>,
    TypedHeader(user_agent): TypedHeader<headers::UserAgent>,
    Path(playlist): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    println!(
        "get: `{}` connected with playlist {}",
        user_agent.as_str(),
        playlist
    );

    let mut db = state.db.clone();
    // let res = db.get(playlist.clone()).await;

    // A `Stream` that repeats an event every second
    //
    // You can also create streams from tokio channels using the wrappers in
    // https://docs.rs/tokio-stream
    let stream = stream::repeat_with(|| Event::default().data("hi!"))
        .map(Ok)
        .throttle(Duration::from_secs(1));

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(1))
            .text("keep-alive-text"),
    )
}

async fn set_playlist(
    state: State<AppState>,
    TypedHeader(user_agent): TypedHeader<headers::UserAgent>,
    Path(playlist): Path<String>,
) -> impl IntoResponse {
    println!(
        "set: `{}` connected with playlist {}",
        user_agent.as_str(),
        playlist
    );

    let mut db = state.db.clone();
    // let res = db.set(playlist.clone(), b"1").await;
    format!("set")
}

#[cfg(test)]
mod tests {
    use {super::*, eventsource_stream::Eventsource, tokio::net::TcpListener};

    #[tokio::test]
    async fn integration_test() {
        // A helper function that spawns our application in the background
        async fn spawn_app(host: impl Into<String>) -> String {
            let host = host.into();
            // Bind to localhost at the port 0, which will let the OS assign an available
            // port to us
            let listener = TcpListener::bind(format!("{}:0", host)).await.unwrap();
            // Retrieve the port assigned to us by the OS
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async {
                axum::serve(listener, app()).await.unwrap();
            });
            // Returns address (e.g. http://127.0.0.1{random_port})
            format!("http://{}:{}", host, port)
        }
        let listening_url = spawn_app("127.0.0.1").await;

        let mut event_stream = reqwest::Client::new()
            .get(&format!("{}/sse", listening_url))
            .header("User-Agent", "integration_test")
            .send()
            .await
            .unwrap()
            .bytes_stream()
            .eventsource()
            .take(1);

        let mut event_data: Vec<String> = vec![];
        while let Some(event) = event_stream.next().await {
            match event {
                Ok(event) => {
                    // break the loop at the end of SSE stream
                    if event.data == "[DONE]" {
                        break;
                    }

                    event_data.push(event.data);
                }
                Err(_) => {
                    panic!("Error in event stream");
                }
            }
        }

        assert!(event_data[0] == "hi!");
    }
}
