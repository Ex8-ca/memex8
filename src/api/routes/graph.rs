use crate::api::server::AppState;
use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct GraphTraverseParams {
    pub memory_id: String,
    pub depth: Option<usize>,
}

#[derive(Serialize)]
pub struct GraphTraverseResponse {
    pub memory_id: String,
    pub results: Vec<GraphResult>,
    pub total: usize,
}

#[derive(Serialize)]
pub struct GraphResult {
    pub memory_id: String,
    pub depth: usize,
    pub path: Vec<GraphStep>,
    pub relevance: f32,
    pub content_preview: String,
}

#[derive(Serialize)]
pub struct GraphStep {
    pub from_id: String,
    pub to_id: String,
    pub relation_type: String,
    pub weight: f32,
}

#[derive(Serialize)]
pub struct GraphStatsResponse {
    pub total_edges: usize,
    pub edge_types: std::collections::HashMap<String, usize>,
    pub unique_entities: usize,
}

#[derive(Serialize)]
pub struct GraphNeighborsResponse {
    pub memory_id: String,
    pub neighbors: Vec<NeighborInfo>,
}

#[derive(Serialize)]
pub struct NeighborInfo {
    pub memory_id: String,
    pub relation_type: String,
    pub weight: f32,
    pub content_preview: String,
}

/// GET /api/v1/graph/traverse?memory_id=&depth=
pub async fn traverse(
    State(state): State<Arc<AppState>>,
    Query(params): Query<GraphTraverseParams>,
) -> Result<Json<GraphTraverseResponse>, crate::api::error::ApiError> {
    let depth = params.depth.unwrap_or(3);

    let results = state.engine.graph_traverse(&params.memory_id, depth).await?;

    let graph_results: Vec<GraphResult> = results
        .into_iter()
        .map(|r| GraphResult {
            memory_id: r.memory_id.clone(),
            depth: r.depth,
            path: r
                .path
                .into_iter()
                .map(|s| GraphStep {
                    from_id: s.from_id,
                    to_id: s.to_id,
                    relation_type: s.relation_type,
                    weight: s.weight,
                })
                .collect(),
            relevance: r.relevance,
            content_preview: String::new(), // Will be populated below
        })
        .collect();

    // Fetch content previews for each result
    let mut enriched_results = Vec::new();
    for mut gr in graph_results {
        if let Ok(memory) = state.engine.get_memory(&gr.memory_id).await {
            gr.content_preview = memory.content.chars().take(200).collect();
        }
        enriched_results.push(gr);
    }

    let total = enriched_results.len();

    Ok(Json(GraphTraverseResponse {
        memory_id: params.memory_id,
        results: enriched_results,
        total,
    }))
}

/// GET /api/v1/graph/stats
pub async fn stats(
    State(state): State<Arc<AppState>>,
) -> Result<Json<GraphStatsResponse>, crate::api::error::ApiError> {
    let stats = state.engine.graph_stats().await?;
    Ok(Json(GraphStatsResponse {
        total_edges: stats.total_edges,
        edge_types: stats.edge_types,
        unique_entities: stats.unique_entities,
    }))
}

/// GET /api/v1/graph/neighbors?memory_id=
pub async fn neighbors(
    State(state): State<Arc<AppState>>,
    Query(params): Query<NeighborsParams>,
) -> Result<Json<GraphNeighborsResponse>, crate::api::error::ApiError> {
    let relationships = state.engine.graph_neighbors(&params.memory_id).await?;

    let mut neighbors = Vec::new();
    for rel in &relationships {
        let other_id = if rel.from_id == params.memory_id {
            &rel.to_id
        } else {
            &rel.from_id
        };

        let content_preview = state
            .engine
            .get_memory(other_id)
            .await
            .map(|m| m.content.chars().take(200).collect())
            .unwrap_or_default();

        neighbors.push(NeighborInfo {
            memory_id: other_id.clone(),
            relation_type: rel.relation_type.clone(),
            weight: rel.weight,
            content_preview,
        });
    }

    Ok(Json(GraphNeighborsResponse {
        memory_id: params.memory_id,
        neighbors,
    }))
}

#[derive(Deserialize)]
pub struct NeighborsParams {
    pub memory_id: String,
}

/// POST /api/v1/graph/build
pub async fn build(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, crate::api::error::ApiError> {
    let threshold = state.engine.config().slumber.association_min_strength;
    let count = state.engine.build_graph(threshold).await?;
    Ok(Json(serde_json::json!({
        "status": "built",
        "edges_created": count,
    })))
}
