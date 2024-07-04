use {
    crate::{model::PlaylistData, routes::internal_error, AppState},
    anyhow::{anyhow, Result},
    axum::{
        extract::{Path, State},
        http::StatusCode,
        response::{
            sse::{Event, Sse},
            IntoResponse,
        },
        Json,
    },
    bb8_redis::redis::AsyncCommands,
    futures::{stream::Stream, StreamExt},
    serde_json::json,
    std::{convert::Infallible, time::Duration},
    tokio_stream::wrappers::BroadcastStream,
};

pub async fn get_playlist(
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

pub async fn set_playlist(
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
                internal_error(anyhow!(err))
            }
        },
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            internal_error(anyhow!(err))
        }
    }
}
