use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::Response,
};
use reqwest::Client;

pub async fn proxy_handler(
    State(client): State<Client>,
    req: Request<Body>,
) -> Result<Response<Body>, (StatusCode, String)> {
    let path = req.uri().path().strip_prefix("/v1/proxy/").unwrap_or("");
    let query = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();
    
    let url = format!("https://{}{}", path, query);

    let client_req = client
        .request(req.method().clone(), &url)
        .headers(req.headers().clone())
        .body(req.into_body());

    match client_req.send().await {
        Ok(res) => {
            let mut response_builder = Response::builder()
                .status(res.status());

            for (name, value) in res.headers() {
                response_builder = response_builder.header(name, value);
            }

            let body = axum::body::Body::from_stream(res.bytes_stream());

            Ok(response_builder.body(body).unwrap())
        }
        Err(e) => Err((StatusCode::BAD_GATEWAY, format!("Failed to fetch from upstream server: {}", e))),
    }
}