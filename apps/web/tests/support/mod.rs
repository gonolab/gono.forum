use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use gono_forum_web::config::Config;
use http_body_util::BodyExt;
use tower::ServiceExt;

pub fn config() -> Config {
    Config::load_test(&gono_forum_web::project_root()).unwrap()
}

pub async fn fetch(app: &Router, path: &str, htmx: bool) -> (StatusCode, HeaderMap, String) {
    let request = Request::builder()
        .uri(path)
        .header("HX-Request", if htmx { "true" } else { "false" })
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    (status, headers, body)
}

#[allow(dead_code)]
pub fn config_for(db: &gono_forum_web::db::Database) -> Config {
    let mut config = config();
    config.redis.namespace = db
        .pool
        .database_name()
        .expect("tests must use a named isolated database");
    config
}
