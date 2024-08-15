use {
    crate::AppState,
    axum::{
        extract::State,
        http::{header, Request, StatusCode},
        response::Response,
    },
};

pub async fn proxy_handler(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
) -> Result<Response<axum::body::Body>, (StatusCode, String)> {
    let path = req.uri().path().strip_prefix("/v1/proxy/").unwrap_or("");
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();

    let url = format!("https://{}{}", path, query);

    // Remove headers that might cause issues
    req.headers_mut().remove(header::HOST);
    req.headers_mut().remove(header::ORIGIN);
    req.headers_mut().remove(header::REFERER);

    // Add or modify headers as needed
    req.headers_mut().insert(
        header::USER_AGENT,
        header::HeaderValue::from_static("YourProxyUserAgent/1.0"),
    );

    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read request body: {}", e),
        )
    })?;

    let client_req = state
        .client
        .request(parts.method, &url)
        .headers(parts.headers)
        .body(bytes);

    match client_req.send().await {
        Ok(res) => {
            let mut response_builder = Response::builder().status(res.status());

            // Forward all headers except those that might cause issues
            for (name, value) in res.headers() {
                if name != header::TRANSFER_ENCODING {
                    response_builder = response_builder.header(name, value);
                }
            }

            // Ensure CORS headers are set
            response_builder = response_builder
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS")
                .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "*");

            let body = axum::body::Body::from_stream(res.bytes_stream());

            Ok(response_builder.body(body).unwrap())
        }
        Err(e) => {
            eprintln!("Proxy error: {}", e);
            Err((
                StatusCode::BAD_GATEWAY,
                format!("Failed to fetch from upstream server: {}", e),
            ))
        }
    }
}