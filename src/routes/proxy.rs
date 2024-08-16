use {
    crate::AppState,
    axum::{
        body::Body,
        extract::State,
        http::{header, Request, StatusCode},
        response::Response,
    },
    reqwest::{redirect::Policy, Client},
};

pub async fn proxy_handler(
    State(_state): State<AppState>,
    req: Request<Body>,
) -> Result<Response<Body>, (StatusCode, String)> {
    let path = req.uri().path().strip_prefix("/v1/proxy/").unwrap_or("");
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();

    // Correctly reconstruct the URL
    let url = if path.starts_with("http://") || path.starts_with("https://") {
        format!("{}{}", path, query)
    } else {
        format!("https://{}{}", path, query)
    };
    println!("Proxying request to: {}", url);

    // Create a client that follows redirects
    let client = Client::builder()
        .redirect(Policy::limited(10))  // Allow up to 10 redirects
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create client: {}", e)))?;

    let mut proxy_req = client.request(req.method().clone(), &url);

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

    // Send the request
    match proxy_req.send().await {
        Ok(res) => {
            println!(
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

            Ok(response_builder.body(body).unwrap())
        }
        Err(e) => {
            println!("Proxy error: {:?}", e);
            Err((
                StatusCode::BAD_GATEWAY,
                format!("Failed to fetch from upstream server: {:?}", e),
            ))
        }
    }
}
