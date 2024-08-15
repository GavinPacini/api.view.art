use {
    crate::AppState,
    axum::{
        extract::State,
        http::{header, Request, StatusCode},
        response::Response,
    },
    reqwest::{redirect::Policy, Client},
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
    let body = hyper::body::to_bytes(req.into_body()).await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to read request body: {}", e),
        )
    })?;
    proxy_req = proxy_req.body(body);

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
                if key != header::TRANSFER_ENCODING {
                    response_builder = response_builder.header(key, value);
                }
            }

            // Set CORS headers
            response_builder = response_builder
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS")
                .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "*");

            let body = axum::body::Body::from_stream(res.bytes_stream());

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
