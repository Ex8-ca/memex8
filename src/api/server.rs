use crate::config::AppConfig;
use crate::engine::Engine;
use axum::Router;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub struct AppState {
    pub engine: Arc<Engine>,
    pub config: AppConfig,
}

pub async fn run(config: AppConfig, host: &str, port: u16) -> anyhow::Result<()> {
    let engine = Arc::new(Engine::new(config.clone()).await?);
    let state = Arc::new(AppState { engine, config });

    let has_key = state.config.api_key().is_some();
    if has_key {
        tracing::info!("🔐 API authentication enabled");
    } else {
        tracing::warn!("⚠️  No MEMEX8_API_KEY set — API is publicly accessible");
    }

    // Auth only on /api/v1 — web UI, health, and MCP are public
    let api_router = if has_key {
        api_routes()
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::api::auth::auth_middleware,
            ))
    } else {
        api_routes()
    };

    // Health and root must be explicit; wildcard must come last
    let app = Router::new()
        .nest("/api/v1", api_router)
        .route("/webhooks/conversation", axum::routing::post(crate::api::routes::webhook::conversation_end))
        .route("/webhooks/skill", axum::routing::post(crate::api::routes::webhook::skill_executed))
        .route("/health", axum::routing::get(health))
        .route("/mcp", axum::routing::get(crate::mcp::http::sse_handler))
        .route("/", axum::routing::get(crate::web::serve_root))
        .fallback(axum::routing::get(crate::web::serve_static))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = format!("{}:{}", host, port);
    tracing::info!("🧠 memex8 server starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/memories", axum::routing::post(crate::api::routes::memories::store))
        .route("/memories/search", axum::routing::post(crate::api::routes::memories::search))
        .route("/memories/recall", axum::routing::get(crate::api::routes::memories::recall))
        .route("/memories/ingest", axum::routing::post(crate::api::routes::memories::ingest))
        .route("/memories/tags", axum::routing::get(crate::api::routes::memories::tags))
        .route("/memories/{id}", axum::routing::get(crate::api::routes::memories::get))
        .route("/memories/{id}", axum::routing::delete(crate::api::routes::memories::delete))
        .route("/memories/{id}/upvote", axum::routing::post(crate::api::routes::memories::upvote))
        .route("/memories/{id}/archive", axum::routing::post(crate::api::routes::memories::archive))
        .route("/realms", axum::routing::get(crate::api::routes::realms::list))
        .route("/realms", axum::routing::post(crate::api::routes::realms::create))
        .route("/realms/{id}", axum::routing::get(crate::api::routes::realms::show))
        .route("/slumber/status", axum::routing::get(crate::api::routes::slumber::status))
        .route("/slumber/trigger", axum::routing::post(crate::api::routes::slumber::trigger))
        .route("/stats", axum::routing::get(crate::api::routes::stats::stats))
        .route("/webhooks/conversation", axum::routing::post(crate::api::routes::webhook::conversation_end))
        .route("/webhooks/skill", axum::routing::post(crate::api::routes::webhook::skill_executed))
        .route("/health", axum::routing::get(health))
}

async fn health() -> &'static str {
    "OK"
}
