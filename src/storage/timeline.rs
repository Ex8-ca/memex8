//! Indexed timeline queries. Equal-time scans use ordinary ID pagination.
use super::*;
use qdrant_client::qdrant::{DatetimeRange, Direction, OrderByBuilder, PointId};

impl QdrantStore {
    pub(crate) async fn timeline_page(
        &self,
        range: DatetimeRange,
        direction: Option<Direction>,
        limit: usize,
        offset: Option<PointId>,
        realm: Option<&str>,
        visibility: Option<&str>,
    ) -> anyhow::Result<(Vec<MemoryPoint>, Option<PointId>)> {
        let mut filter = Self::retrieval_filter(realm, None, visibility);
        filter
            .must
            .push(Condition::datetime_range("ingested_at", range));
        let mut builder = ScrollPointsBuilder::new(MEMORIES)
            .filter(filter)
            .limit(limit as u32)
            .with_payload(true)
            .with_vectors(false);
        if let Some(direction) = direction {
            builder =
                builder.order_by(OrderByBuilder::new("ingested_at").direction(direction.into()));
        }
        if let Some(offset) = offset {
            builder = builder.offset(offset);
        }
        let response = self.client.scroll(builder).await?;
        let rows = response
            .result
            .into_iter()
            .map(|point| {
                let id = point_id_to_string(point.id.as_ref());
                memory_from_payload(&id, &map_to_json(&point.payload))
            })
            .collect();
        Ok((rows, response.next_page_offset))
    }
}
