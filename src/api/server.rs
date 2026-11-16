use crate::config::AppConfig;
use crate::engine::Engine;
use axum::{extract::State, http::Request, middleware::Next, response::Response, Router};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub struct AppState {
    pub engine: Arc<Engine>,
    pub config: AppConfig,
}

/// POST endpoints that fetch memory content and must therefore keep the read
/// gate across handler execution *and* response construction.
///
/// Writer tools stay out of this list. Some of them take the exclusive side of
/// the same gate (certification, edit, deletion) and would deadlock behind the
/// read scope; the rest take no guard but still do not need the read guarantee.
/// `upvote-by-content` belongs here even though it writes: its upvote takes no
/// exclusive maintenance guard, and it returns fetched `content_head`, so its
/// search and response construction must be ordered against erasure. Its own
/// search/get calls take nested read permits (the gate is re-entrant), so
/// holding the read side here cannot deadlock it.
pub fn is_gated_retrieval_post(path: &str) -> bool {
    matches!(
        path,
        "/memories/search" | "/memories/batch-get" | "/memories/upvote-by-content"
    )
}

/// Serialize REST memory reads through response construction, not network delivery.
/// Axum converts `Json` handler results to response bytes within `next.run`.
/// A slow client can still receive previously constructed bytes after deletion.
/// `read_gated` is re-entrant, so the engine's nested read guards cannot queue
/// behind the erasure this middleware is blocking.
pub async fn memory_read_response_gate(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    // Axum strips the `/api/v1` prefix before the nested route middleware.
    // Only these POST methods are reads (or content-publishing reads); do not
    // hold the guard across writers that take the exclusive side.
    let retrieval_post = request.method() == axum::http::Method::POST
        && is_gated_retrieval_post(request.uri().path());
    if request.method() != axum::http::Method::GET && !retrieval_post {
        return next.run(request).await;
    }
    state.engine.store().read_gated(next.run(request)).await
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

    // Fail-closed by default: the auth middleware itself rejects every
    // request to `/api/v1` when MEMEX8_API_KEY is unset, so we always
    // mount it. The previous behavior (skip the middleware entirely
    // when no key was set) silently exposed every API endpoint to
    // anyone who could reach the port — see issue #11.
    //
    // For local development where someone is intentionally probing
    // without auth, the caller passes `allow_no_api_key=true` (CLI flag
    // `--allow-no-api-key` on `memex8 serve`) and we fall back to the
    // old "no middleware" behavior, with a louder warning.
    let allow_no_api_key = state.config.server.allow_no_api_key;
    let has_key = config.api_key().is_some();
    if has_key {
        tracing::info!("🔐 API authentication enabled");
    } else if allow_no_api_key {
        tracing::warn!(
            "⚠️  No MEMEX8_API_KEY set and --allow-no-api-key passed — API is PUBLICLY ACCESSIBLE"
        );
        tracing::warn!(
            "⚠️  This is intended for local development only. Do NOT expose this port to a network."
        );
    } else {
        tracing::warn!(
            "⚠️  No MEMEX8_API_KEY set — every /api/v1 request will return 401. Generate one with:"
        );
        tracing::warn!("⚠️    python3 -c \"import secrets; print(secrets.token_urlsafe(32))\"");
        tracing::warn!(
            "⚠️  To intentionally disable auth (NOT recommended), pass --allow-no-api-key to `memex8 serve`."
        );
    }

    // Auth is always wired in. The middleware is fail-closed: it returns
    // 401 "API key not configured" when no key is set, which is what
    // we want by default. `allow_no_api_key` strips the middleware for
    // local dev only.
    let guarded_routes = api_routes().layer(axum::middleware::from_fn_with_state(
        state.clone(),
        memory_read_response_gate,
    ));
    let api_router = if allow_no_api_key && !has_key {
        guarded_routes
    } else {
        guarded_routes.layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::api::auth::auth_middleware,
        ))
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
        .route(
            "/auth/login",
            axum::routing::post(crate::api::session::login),
        )
        .route(
            "/auth/logout",
            axum::routing::post(crate::api::session::logout),
        )
        .route("/auth/me", axum::routing::get(crate::api::session::me))
        .route("/mcp", axum::routing::get(crate::mcp::http::sse_handler))
        .route("/", axum::routing::get(crate::web::serve_root))
        // Wildcard route for static assets + SPA fallback. Note: this must be
        // a literal `/*path` (or `/{path}`) route, NOT .fallback(), because
        // .fallback() doesn't bind the path parameter and serve_static's
        // Path<String> extractor then errors with "Wrong number of path args".
        .route("/{path}", axum::routing::get(crate::web::serve_static))
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
            "/memories/{id}/evidence/{source_id}",
            axum::routing::get(crate::api::routes::memories::evidence),
        )
        .route(
            "/dreams",
            axum::routing::get(crate::api::routes::dreams::list)
                .post(crate::api::routes::dreams::create),
        )
        .route(
            "/dreams/{id}",
            axum::routing::get(crate::api::routes::dreams::get)
                .delete(crate::api::routes::dreams::delete),
        )
        .route(
            "/dreams/{id}/keep",
            axum::routing::post(crate::api::routes::dreams::keep),
        )
        .route(
            "/settings",
            axum::routing::get(crate::api::settings::get)
                .put(crate::api::settings::put)
                .delete(crate::api::settings::reset),
        )
        .route(
            "/settings/restart",
            axum::routing::post(crate::api::settings::restart),
        )
        .route(
            "/dedup/{id}/restore",
            axum::routing::post(crate::api::routes::memories::restore_duplicate),
        )
        .route(
            "/memories/{id}/unconsolidate",
            axum::routing::post(crate::api::routes::memories::unconsolidate),
        )
        .route(
            "/memories/{id}/certify",
            axum::routing::post(crate::api::routes::memories::certify)
                .delete(crate::api::routes::memories::revoke_certification),
        )
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
            axum::routing::post(crate::api::routes::memories::search)
                .get(crate::api::routes::memories::search_get),
        )
        .route(
            "/memories/timeline",
            axum::routing::get(crate::api::routes::memories::timeline),
        )
        .route(
            "/memories/batch-get",
            axum::routing::post(crate::api::routes::memories::batch_get),
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
            "/memories/public",
            axum::routing::get(crate::api::routes::memories::public_memories),
        )
        .route(
            "/memories/upvote-by-content",
            axum::routing::post(crate::api::routes::memories::upvote_by_content),
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
            "/slumber/pause",
            axum::routing::post(crate::api::routes::slumber::pause),
        )
        .route(
            "/slumber/resume",
            axum::routing::post(crate::api::routes::slumber::resume),
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
