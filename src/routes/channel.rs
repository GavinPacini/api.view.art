use {
    super::auth::Claims,
    crate::{
        model::{ChannelContent, EmptyChannelContent},
        routes::internal_error,
        utils::url_factory::generate_url_from_address,
        AppState,
    },
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
    std::{collections::HashSet, convert::Infallible, time::Duration},
    tokio_stream::wrappers::BroadcastStream,
};

pub const CHANNEL_KEY: &str = "channel";

pub async fn get_channel(
    state: State<AppState>,
    Path(channel): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    tracing::info!("get content for channel {}", channel);

    let key = format!("{}:{}", CHANNEL_KEY, channel);

    // get content from DB if set
    let initial_content: Option<ChannelContent> = {
        match state.pool.get().await {
            Ok(mut conn) => match conn.get(&key).await {
                Ok(content) => Some(content),
                Err(err) => {
                    tracing::error!("Error getting content for channel {}: {:?}", channel, err);
                    None
                }
            },
            Err(err) => {
                tracing::error!("Error getting connection from pool: {:?}", err);
                None
            }
        }
    };

    // subscribe to changes
    let (tx, rx) = {
        let mut changes = state.changes.clone();
        changes.subscribe(channel.clone()).await
    };

    // map changes of channel content to SSE events
    let stream = BroadcastStream::new(rx).map(|content| {
        let event = match content {
            Ok(content) => match content {
                Some(content) => Event::default()
                    .json_data(content)
                    .unwrap()
                    .event("content"),
                None => Event::default()
                    .json_data(EmptyChannelContent::default())
                    .unwrap()
                    .event("content"),
            },
            Err(err) => {
                tracing::error!("Error getting content: {:?}", err);
                Event::default()
            }
        };

        Ok::<Event, Infallible>(event)
    });

    // send initial content to subscribers
    match tx.send(initial_content) {
        Ok(len) => {
            tracing::debug!("sent {} to {} receivers", channel, len);
        }
        Err(err) => {
            tracing::error!("Error sending initial content: {:?}", err);
        }
    }

    // return SSE stream
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(1))
            .text("keep-alive-text"),
    )
}

pub async fn set_channel(
    claims: Claims,
    state: State<AppState>,
    Path(channel): Path<String>,
    Json(channel_content): Json<ChannelContent>,
) -> impl IntoResponse {
    tracing::info!(
        "set channel content for {}, owned by {:?}",
        channel,
        claims.address
    );

    if let Err(err) = validate_channel_content(&channel_content) {
        tracing::debug!("Error validating channel content: {:?}", err);
        return (
            StatusCode::BAD_REQUEST,
            json!({ "status": false, "error": err.to_string() }).to_string(),
        );
    }

    // Currently admin can set any channel
    // TODO: Investigate if we want this or not
    if !claims.address.is_zero() {
        let channel_url = generate_url_from_address(claims.address);
        if channel != channel_url {
            return (
                StatusCode::BAD_REQUEST,
                json!({ "status": false, "error": "invalid channel" }).to_string(),
            );
        }
    }

    let key = format!("{}:{}", CHANNEL_KEY, channel);

    match state.pool.get().await {
        Ok(mut conn) => match conn
            .set::<&str, String, ()>(&key, serde_json::to_string(&channel_content).unwrap())
            .await
        {
            Ok(_) => {
                state.changes.broadcast(&channel, channel_content).await;
                (StatusCode::OK, json!({ "status": true }).to_string())
            }
            Err(err) => {
                tracing::error!("Error setting content for channel {}: {:?}", channel, err);
                internal_error(anyhow!(err))
            }
        },
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            internal_error(anyhow!(err))
        }
    }
}

fn validate_channel_content(channel_content: &ChannelContent) -> Result<()> {
    // ensure all ids are unique in items
    let mut ids = HashSet::new();
    for item in &channel_content.items {
        if ids.contains(&item.id) {
            return Err(anyhow!("duplicate item id: {}", item.id));
        }
        ids.insert(item.id.clone());
    }

    for item in &channel_content.items {
        if item.title.is_empty() || item.title.len() > 100 {
            return Err(anyhow!(
                "item title must be between 1 and 100 characters, for id {}",
                item.id
            ));
        }

        if let Some(artist) = &item.artist {
            if artist.is_empty() || artist.len() > 100 {
                return Err(anyhow!(
                    "item artist must be omitted or between 1 and 100 characters, for id {}",
                    item.id
                ));
            }
        }
    }

    Ok(())
}
