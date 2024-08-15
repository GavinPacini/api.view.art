use {
    crate::AppState,
    axum::{
        extract::State,
        http::{Request, StatusCode},
        response::Response,
    },
};

pub async fn proxy_handler(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
) -> Result<Response<axum::body::Body>, (StatusCode, String)> {
    let path = req.uri().path().strip_prefix("/v1/proxy/").unwrap_or("");
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    
    let url = format!("https://{}{}", path, query);

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

            for (name, value) in res.headers() {
                response_builder = response_builder.header(name, value);
            }

            let body = axum::body::Body::from_stream(res.bytes_stream());

            Ok(response_builder.body(body).unwrap())
        }
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            format!("Failed to fetch from upstream server: {}", e),
        )),
    }
}