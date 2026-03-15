mod metrics;
mod models;
mod pipeline;

use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use tracing::{error, info};

use metrics::{Metrics, RequestLog};
use models::{SolveRequest, StatusResponse};
use pipeline::gemini::GeminiClient;

struct AppState {
    gemini: GeminiClient,
    metrics: Arc<Metrics>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let api_key =
        std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable required");

    let metrics = Arc::new(Metrics::new());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(240))
        .build()
        .expect("Failed to build HTTP client");

    let gemini = GeminiClient::new(client, api_key, metrics.clone());
    let state = Arc::new(AppState { gemini, metrics });

    let app = Router::new()
        .route("/", get(health))
        .route("/metrics", get(get_metrics))
        .route("/metrics/reset", post(reset_metrics))
        .route("/solve", post(solve))
        .route("/analytics", get(get_analytics))
        .with_state(state);

    let addr = "0.0.0.0:8080";
    info!("Listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> Json<StatusResponse> {
    Json(StatusResponse {
        status: "ok".to_string(),
    })
}

async fn get_metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.metrics.get())
}

async fn reset_metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    state.metrics.reset();
    Json(StatusResponse {
        status: "reset".to_string(),
    })
}

async fn solve(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<SolveRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Json(request) = match payload {
        Ok(json) => json,
        Err(rejection) => {
            error!("Failed to parse /solve request: {rejection}");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("{rejection}")})),
            );
        }
    };
    let segment = request.segment.clone();
    let num_offers = request.offers.len();
    let num_fields = request.fields_to_extract.len();

    info!("Solving: segment={segment}, offers={num_offers}, fields={num_fields}");

    let (pre_calls, pre_prompt, pre_comp, _) = state.metrics.snapshot();
    let start = std::time::Instant::now();

    let response = pipeline::solve(request, &state.gemini).await;

    let elapsed = start.elapsed();
    let (post_calls, post_prompt, post_comp, _post_total) = state.metrics.snapshot();

    let log = RequestLog {
        segment: segment.clone(),
        num_offers,
        num_fields,
        elapsed_ms: elapsed.as_millis() as u64,
        gemini_calls: post_calls - pre_calls,
        prompt_tokens: post_prompt - pre_prompt,
        completion_tokens: post_comp - pre_comp,
        total_tokens: (post_prompt - pre_prompt) + (post_comp - pre_comp),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    info!(
        "Solved {segment} in {}ms: {} Gemini calls, {} tokens",
        log.elapsed_ms, log.gemini_calls, log.total_tokens
    );

    state.metrics.log_request(log);
    (StatusCode::OK, Json(serde_json::to_value(response).unwrap()))
}

async fn get_analytics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let logs = state.metrics.get_logs();
    let metrics = state.metrics.get();

    let analytics = serde_json::json!({
        "summary": metrics,
        "requests": logs,
        "total_requests": logs.len(),
    });

    Json(analytics)
}
