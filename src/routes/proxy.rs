use {
    crate::AppState,
    axum::{
        body::Body,
        extract::State,
        http::{header, Request, StatusCode},
        response::Response,
    },
    reqwest::{redirect::Policy, Client},
    url::Url,
};

pub async fn proxy_handler(
    State(_state): State<AppState>,
    req: Request<Body>,
) -> Result<Response<Body>, (StatusCode, String)> {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("");
    tracing::debug!("Received Path and Query: {}", path_and_query);

    // Ensure the correct prefix is stripped
    let path = match path_and_query.strip_prefix("/proxy/") {
        Some(p) => p,
        None => {
            return Err((StatusCode::BAD_REQUEST, "Invalid request path".to_string()));
        }
    };

    tracing::debug!("Path after stripping prefix: {}", path);

    // Attempt to parse the full URL
    let url = match Url::parse(path) {
        Ok(url) => url,
        Err(e) => {
            tracing::error!("URL parsing error: {}", e);
            return Err((StatusCode::BAD_REQUEST, "Invalid URL".to_string()));
        }
    };

    tracing::debug!(
        "Parsed URL - Scheme: {},Host: {:?}, Path: {}, Query: {:?}",
        url.scheme(),
        url.host(),
        url.path(),
        url.query()
    );

    tracing::debug!("Proxying request to: {}", url);

    // Create a client that follows redirects
    let client = Client::builder()
        .redirect(Policy::limited(10))  // Allow up to 10 redirects
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create client: {}", e)))?;

    let mut proxy_req = client.request(req.method().clone(), url);

    // Forward headers
    for (key, value) in req.headers() {
        if key != header::HOST {
            proxy_req = proxy_req.header(key, value);
        }
    }

    // Set the body
    let body_bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("Failed to read request body: {}", e),
            )
        })?;
    proxy_req = proxy_req.body(body_bytes);

    tracing::debug!("Sending request: {:?}", proxy_req);

    // Send the request
    match proxy_req.send().await {
        Ok(res) => {
            tracing::debug!(
                "Received response from upstream with status: {}",
                res.status()
            );

            let mut response_builder = Response::builder().status(res.status());

            // Forward headers
            for (key, value) in res.headers() {
                if key != header::TRANSFER_ENCODING && key != header::CONTENT_LENGTH {
                    response_builder = response_builder.header(key, value);
                }
            }

            // Set CORS headers
            response_builder = response_builder
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS")
                .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "*");

            // Convert the response body to axum's Body type
            let body_bytes = res.bytes().await.map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("Failed to read response body: {}", e),
                )
            })?;
            let body = Body::from(body_bytes);

            match response_builder.body(body) {
                Ok(response) => Ok(response),
                Err(e) => {
                    tracing::error!("Failed to build response: {}", e);
                    Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to build response".to_string(),
                    ))
                }
            }
        }
        Err(e) => {
            tracing::error!("Proxy error: {:?}", e);
            Err((
                StatusCode::BAD_GATEWAY,
                format!("Failed to fetch from upstream server: {:?}", e),
            ))
        }
    }
}
