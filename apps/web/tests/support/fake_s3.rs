//! An S3 stub only for integration tests: PUT stores into memory, GET, HEAD and DELETE read and write by request path.
//! No signature checks; the path is `/<bucket>/<key>` (the storage config uses path style).
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};

type Objects = Arc<Mutex<HashMap<String, Vec<u8>>>>;

#[derive(Clone, Default)]
struct Stub {
    objects: Objects,
    /// While set, every GET and HEAD answers 403: storage temporarily unreadable (without triggering SDK retries).
    fail_reads: Arc<AtomicBool>,
}

pub struct FakeS3 {
    pub endpoint: String,
    objects: Objects,
    fail_reads: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeS3 {
    pub async fn new() -> Self {
        let stub = Stub::default();
        let objects = stub.objects.clone();
        let fail_reads = stub.fail_reads.clone();
        let router = Router::new().fallback(object).with_state(stub);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            endpoint,
            objects,
            fail_reads,
            task,
        }
    }

    /// Makes reads fail (`true`) or work again (`false`).
    #[allow(dead_code)] // Only some test binaries use it
    pub fn fail_reads(&self, fail: bool) {
        self.fail_reads.store(fail, Ordering::SeqCst);
    }

    /// The single object whose path ends with `suffix`.
    pub fn object_ending_with(&self, suffix: &str) -> Vec<u8> {
        let objects = self.objects.lock().unwrap();
        let matches: Vec<_> = objects
            .iter()
            .filter(|(path, _)| path.ends_with(suffix))
            .collect();
        assert_eq!(matches.len(), 1, "no single object ends with {suffix}");
        matches[0].1.clone()
    }

    /// How many objects have a path ending with `suffix`.
    #[allow(dead_code)] // Only some test binaries use it
    pub fn count_ending_with(&self, suffix: &str) -> usize {
        let objects = self.objects.lock().unwrap();
        objects.keys().filter(|path| path.ends_with(suffix)).count()
    }

    /// Deletes objects whose path ends with `suffix` (simulating old images that have no thumbnail yet).
    #[allow(dead_code)] // Only some test binaries use it
    pub fn remove_ending_with(&self, suffix: &str) {
        self.objects
            .lock()
            .unwrap()
            .retain(|path, _| !path.ends_with(suffix));
    }
}

impl Drop for FakeS3 {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn object(State(stub): State<Stub>, request: Request) -> Response {
    let Stub {
        objects,
        fail_reads,
    } = stub;
    let path = request.uri().path().to_owned();
    match *request.method() {
        Method::GET | Method::HEAD if fail_reads.load(Ordering::SeqCst) => {
            StatusCode::FORBIDDEN.into_response()
        }
        Method::PUT => {
            let bytes = to_bytes(request.into_body(), 64 * 1024 * 1024)
                .await
                .unwrap();
            objects.lock().unwrap().insert(path, bytes.to_vec());
            (StatusCode::OK, [("etag", "\"fake-etag\"")]).into_response()
        }
        Method::GET => match objects.lock().unwrap().get(&path).cloned() {
            Some(bytes) => (
                [("content-type", "application/octet-stream")],
                Body::from(bytes),
            )
                .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        Method::HEAD => {
            if objects.lock().unwrap().contains_key(&path) {
                StatusCode::OK.into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
        Method::DELETE => {
            objects.lock().unwrap().remove(&path);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}
