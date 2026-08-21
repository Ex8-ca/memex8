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
    run_with_engine(config, engine, host, port).await
}

/// Start the API server with a pre-built engine. Used when serve mode also
/// needs to run the scheduler + watchers (combined serve+daemon).
pub async fn run_with_engine(
    config: AppConfig,
    engine: Arc<Engine>,
    host: &str,
    port: u16,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState {
        engine,
        config: config.clone(),
    });

    // Inject the API key into the web UI at serve time
    crate::web::init(config.api_key());

    let has_key = config.api_key().is_some();
    if has_key {
        tracing::info!("🔐 API authentication enabled");
    } else {
        tracing::warn!("⚠️  No MEMEX8_API_KEY set — API is publicly accessible");
    }

    // Auth only on /api/v1 — web UI, health, and MCP are public
    let api_router = if has_key {
        api_routes().layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::api::auth::auth_middleware,
        ))
    } else {
        api_routes()
    };

    // Health and root must be explicit; wildcard must come last
    let app = Router::new()
        .nest("/api/v1", api_router)
        .route(
            "/webhooks/conversation",
            axum::routing::post(crate::api::routes::webhook::conversation_end),
        )
        .route(
            "/webhooks/skill",
            axum::routing::post(crate::api::routes::webhook::skill_executed),
        )
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
        .route(
            "/memories",
            axum::routing::get(crate::api::routes::memories::list),
        )
        .route(
            "/memories",
            axum::routing::post(crate::api::routes::memories::store),
        )
        .route(
            "/memories/search",
            axum::routing::post(crate::api::routes::memories::search),
        )
        .route(
            "/memories/recall",
            axum::routing::get(crate::api::routes::memories::recall),
        )
        .route(
            "/memories/ingest",
            axum::routing::post(crate::api::routes::memories::ingest),
        )
        .route(
            "/memories/tags",
            axum::routing::get(crate::api::routes::memories::tags),
        )
        .route(
            "/memories/verification-summary",
            axum::routing::get(crate::api::routes::memories::verification_summary),
        )
        .route(
            "/memories/{id}",
            axum::routing::get(crate::api::routes::memories::get),
        )
        .route(
            "/memories/{id}",
            axum::routing::delete(crate::api::routes::memories::delete),
        )
        .route(
            "/memories/{id}",
            axum::routing::patch(crate::api::routes::memories::update_memory),
        )
        .route(
            "/memories/{id}/upvote",
            axum::routing::post(crate::api::routes::memories::upvote),
        )
        .route(
            "/memories/{id}/downvote",
            axum::routing::post(crate::api::routes::memories::downvote),
        )
        .route(
            "/memories/{id}/archive",
            axum::routing::post(crate::api::routes::memories::archive),
        )
        .route(
            "/realms",
            axum::routing::get(crate::api::routes::realms::list),
        )
        .route(
            "/realms",
            axum::routing::post(crate::api::routes::realms::create),
        )
        .route(
            "/realms/{id}",
            axum::routing::get(crate::api::routes::realms::show),
        )
        .route(
            "/slumber/status",
            axum::routing::get(crate::api::routes::slumber::status),
        )
        .route(
            "/slumber/trigger",
            axum::routing::post(crate::api::routes::slumber::trigger),
        )
        .route(
            "/stats",
            axum::routing::get(crate::api::routes::stats::stats),
        )
        .route(
            "/webhooks/conversation",
            axum::routing::post(crate::api::routes::webhook::conversation_end),
        )
        .route(
            "/webhooks/skill",
            axum::routing::post(crate::api::routes::webhook::skill_executed),
        )
        .route(
            "/inference/suggest",
            axum::routing::post(crate::api::routes::inference::suggest),
        )
        .route(
            "/inference/gaps",
            axum::routing::get(crate::api::routes::inference::list_gaps),
        )
        .route(
            "/inference/gaps/{id}/resolve",
            axum::routing::post(crate::api::routes::inference::resolve_gap),
        )
        .route(
            "/inference/gaps/{id}/dismiss",
            axum::routing::post(crate::api::routes::inference::dismiss_gap),
        )
        .route(
            "/sessions/end",
            axum::routing::post(crate::api::routes::session::session_end),
        )
        .route(
            "/graph/traverse",
            axum::routing::get(crate::api::routes::graph::traverse),
        )
        .route(
            "/graph/stats",
            axum::routing::get(crate::api::routes::graph::stats),
        )
        .route(
            "/graph/neighbors",
            axum::routing::get(crate::api::routes::graph::neighbors),
        )
        .route(
            "/graph/build",
            axum::routing::post(crate::api::routes::graph::build),
        )
        .route("/health", axum::routing::get(health))
}

async fn health() -> &'static str {
    "OK"
}
