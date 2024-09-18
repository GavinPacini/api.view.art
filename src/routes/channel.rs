use {
    super::auth::Claims,
    crate::{
        model::{ChannelContent, EmptyChannelContent, OldChannelContent, Status},
        routes::internal_error,
        utils::{
            address_migration::migrate_addresses,
            keys::{address_key, channel_key},
        },
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

pub async fn get_channel(
    state: State<AppState>,
    Path(channel): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let channel = channel.to_ascii_lowercase();

    tracing::info!("get content for channel {}", channel);

    let key = channel_key(&channel);

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

pub async fn is_taken(state: State<AppState>, Path(channel): Path<String>) -> impl IntoResponse {
    let channel = channel.to_ascii_lowercase();

    tracing::info!("check if channel {} is taken", channel);

    let key = channel_key(&channel);

    let exists: bool = match state.pool.get().await {
        Ok(mut conn) => match conn.exists::<&str, bool>(&key).await {
            Ok(exists) => exists,
            Err(err) => {
                tracing::error!("Error checking if channel exists: {:?}", err);
                return internal_error(anyhow!(err));
            }
        },
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            return internal_error(anyhow!(err));
        }
    };

    (StatusCode::OK, json!({ "taken": exists }).to_string())
}

pub async fn get_summary(state: State<AppState>, Path(channel): Path<String>) -> impl IntoResponse {
    let channel = channel.to_ascii_lowercase();

    tracing::info!("get summary for channel {}", channel);

    let key = channel_key(&channel);

    let initial_content: Option<ChannelContent> = {
        match state.pool.get().await {
            Ok(mut conn) => match conn.get::<&str, String>(key.as_str()).await {
                Ok(content) => {
                    // Try to deserialize to the new format
                    match serde_json::from_str::<ChannelContent>(&content) {
                        Ok(new_content) => Some(new_content),
                        Err(_) => {
                            // If deserialization fails, try to deserialize to the old format
                            match serde_json::from_str::<OldChannelContent>(&content) {
                                Ok(old_content) => {
                                    // Convert the old content to the new format
                                    Some(ChannelContent {
                                        items: old_content.items,
                                        status: Status {
                                            item: old_content.played.item,
                                            at: old_content.played.at,
                                            action: "played".to_string(), /* Default action based
                                                                           * on old format */
                                        },
                                    })
                                }
                                Err(err) => {
                                    tracing::error!(
                                        "Error deserializing content for channel {}: {:?}",
                                        channel,
                                        err
                                    );
                                    None
                                }
                            }
                        }
                    }
                }
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

    match initial_content {
        Some(content) => {
            // Make summary a default JSON object
            let mut summary = json!({});

            // Add items to summary
            summary["items"] = json!(content.items.len());

            // Add thumbnail to summary
            if let Some(thumbnail) = content.items.first().map(|item| item.thumbnail_url.clone()) {
                summary["thumbnail"] = json!(thumbnail);
            }

            (StatusCode::OK, json!(summary).to_string())
        }
        None => (
            StatusCode::NOT_FOUND,
            json!({ "status": false, "error": "channel not found" }).to_string(),
        ),
    }
}

pub async fn set_channel(
    claims: Claims,
    state: State<AppState>,
    Path(channel): Path<String>,
    Json(channel_content): Json<ChannelContent>,
) -> impl IntoResponse {
    let channel = channel.to_ascii_lowercase();

    tracing::info!(
        "set channel content for {}, owned by {:?}",
        channel,
        claims.address
    );

    if !channel
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return (
            StatusCode::BAD_REQUEST,
            json!({ "status": false, "error": "invalid channel name, must be ascii alphabetic" })
                .to_string(),
        );
    }

    if let Err(err) = migrate_addresses(&claims.address, state.pool.get().await).await {
        tracing::error!("Error migrating address key: {:?}", err);
        return internal_error(anyhow!(err));
    }

    // check if the user is an admin or the channel is owned by the user
    let address_key = address_key(&claims.address);
    let owned: bool = if claims.address.is_zero() {
        // TODO: currently admin can set any channel, investigate if we want this or not
        // if admin, always set owned to true
        true
    } else {
        match state.pool.get().await {
            Ok(mut conn) => match conn
                .sismember::<&str, &str, bool>(&address_key, &channel)
                .await
            {
                Ok(owned) => owned,
                Err(err) => {
                    tracing::error!("Error checking if channel is owned: {:?}", err);
                    return internal_error(anyhow!(err));
                }
            },
            Err(err) => {
                tracing::error!("Error getting connection from pool: {:?}", err);
                return internal_error(anyhow!(err));
            }
        }
    };

    // check if channel already exists
    let channel_key = channel_key(&channel);
    let exists: bool = match state.pool.get().await {
        Ok(mut conn) => match conn.exists::<&str, bool>(&channel_key).await {
            Ok(exists) => exists,
            Err(err) => {
                tracing::error!("Error checking if channel exists: {:?}", err);
                return internal_error(anyhow!(err));
            }
        },
        Err(err) => {
            tracing::error!("Error getting connection from pool: {:?}", err);
            return internal_error(anyhow!(err));
        }
    };

    if exists && !owned {
        return (
            StatusCode::BAD_REQUEST,
            json!({ "status": false, "error": "channel already exists" }).to_string(),
        );
    }

    if let Err(err) = validate_channel_content(&channel_content) {
        tracing::debug!("Error validating channel content: {:?}", err);
        return (
            StatusCode::BAD_REQUEST,
            json!({ "status": false, "error": err.to_string() }).to_string(),
        );
    }

    match state.pool.get().await {
        Ok(mut conn) => match conn.sadd::<&str, &str, ()>(&address_key, &channel).await {
            Ok(_) => match conn
                .set::<&str, String, ()>(
                    &channel_key,
                    serde_json::to_string(&channel_content).unwrap(),
                )
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
                tracing::error!(
                    "Error adding channel {} to owner set {}: {:?}",
                    channel,
                    claims.address,
                    err
                );
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
