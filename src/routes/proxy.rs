use {
    crate::AppState,
    axum::{
        extract::State,
        http::{HeaderMap, HeaderValue, Request, StatusCode},
        response::Response,
    },
    hyper::header,
    url::Url,
};

pub async fn proxy_handler(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
) -> Result<Response<axum::body::Body>, (StatusCode, String)> {
    let path = req.uri().path().strip_prefix("/v1/proxy/").unwrap_or("");
    let query = req.uri().query().unwrap_or("");

    let url = match Url::parse(&format!("{}{}?{}", path, query)) {
        Ok(url) => url,
        Err(e) => {
            tracing::error!("Failed to parse URL: {}", e);
            return Err((StatusCode::BAD_REQUEST, format!("Invalid URL: {}", e)));
        }
    };

    tracing::debug!("Proxying request to: {}", url);

    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.map_err(|e| {
        tracing::error!("Failed to read request body: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read request body: {}", e),
        )
    })?;

    let mut client_req = state
        .client
        .request(parts.method, url)
        .body(bytes);

    // Forward relevant headers
    for (key, value) in parts.headers.iter() {
        if !key.as_str().to_lowercase().starts_with("x-") 
           && key != header::HOST 
           && key != header::CONTENT_LENGTH {
            client_req = client_req.header(key, value);
        }
    }

    match client_req.send().await {
        Ok(res) => {
            let mut response_builder = Response::builder().status(res.status());

            // Add CORS headers
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
            headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, OPTIONS"));
            headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("Content-Type"));

            // Forward relevant headers from the upstream response
            for (key, value) in res.headers() {
                if !key.as_str().to_lowercase().starts_with("x-") {
                    headers.insert(key, value.clone());
                }
            }

            response_builder = response_builder.headers(headers);

            let body = axum::body::Body::from_stream(res.bytes_stream());

            Ok(response_builder.body(body).unwrap())
        }
        Err(e) => {
            tracing::error!("Failed to fetch from upstream server: {}", e);
            Err((
                StatusCode::BAD_GATEWAY,
                format!("Failed to fetch from upstream server: {}", e),
            ))
        }
    }
}