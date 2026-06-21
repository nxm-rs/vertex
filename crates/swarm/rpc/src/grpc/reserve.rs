//! Reserve service: storer reserve state, radius, capacity, and per-bin order.

use tonic::{Request, Response, Status};
use vertex_swarm_api::BinCursorStore;
use vertex_swarm_primitives::ProximityOrder;

use crate::proto::reserve::{
    GetReserveBinsRequest, GetReserveBinsResponse, GetReserveStateRequest, GetReserveStateResponse,
    ReserveBin, reserve_server::Reserve,
};

/// Reserve service implementation.
pub struct ReserveService<R> {
    reserve: R,
}

impl<R> ReserveService<R> {
    pub fn new(reserve: R) -> Self {
        Self { reserve }
    }
}

#[tonic::async_trait]
impl<R: BinCursorStore + Send + Sync + 'static> Reserve for ReserveService<R> {
    async fn get_reserve_state(
        &self,
        _request: Request<GetReserveStateRequest>,
    ) -> Result<Response<GetReserveStateResponse>, Status> {
        let count = self
            .reserve
            .count()
            .map_err(|e| Status::internal(format!("reserve count failed: {e}")))?;

        Ok(Response::new(GetReserveStateResponse {
            storage_radius: u32::from(self.reserve.storage_radius().get()),
            count,
            capacity: self.reserve.capacity(),
        }))
    }

    async fn get_reserve_bins(
        &self,
        _request: Request<GetReserveBinsRequest>,
    ) -> Result<Response<GetReserveBinsResponse>, Status> {
        let radius = self.reserve.storage_radius();

        // The area of responsibility is `radius..=MAX_PO`: the bins that hold
        // reserve chunks (shallower bins are shed as the radius grows), so their
        // per-bin counts sum to the total reported by GetReserveState.
        let max_po = ProximityOrder::MAX.get();
        let mut bins = Vec::with_capacity(usize::from(max_po - radius.get()) + 1);
        for po_raw in radius.get()..=max_po {
            // `po_raw` is a valid proximity order by construction; fall back to the
            // deepest bin rather than surfacing an impossible error.
            let po = ProximityOrder::new(po_raw).unwrap_or(ProximityOrder::MAX);
            let count = self
                .reserve
                .count_in(po)
                .map_err(|e| Status::internal(format!("reserve bin count failed: {e}")))?;
            let cursor = self
                .reserve
                .bin_cursor(po.into())
                .map_err(|e| Status::internal(format!("reserve bin cursor failed: {e}")))?;
            bins.push(ReserveBin {
                proximity_order: u32::from(po_raw),
                count,
                cursor,
            });
        }

        Ok(Response::new(GetReserveBinsResponse {
            storage_radius: u32::from(radius.get()),
            bins,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vertex_swarm_api::{
        BatchId, BinScanItem, ChunkAddress, ReserveStore, SettableRadius, StorageRadius,
        SwarmLocalStore, SwarmResult,
    };
    use vertex_swarm_primitives::{Bin, CachedChunk, ProximityOrder};

    use super::*;

    /// Fixed-shape reserve: a known radius and capacity, with per-proximity-order
    /// counts from `bin_counts`. `count()` sums `bin_counts`.
    struct FixedReserve {
        radius: StorageRadius,
        bin_counts: Vec<u64>,
        capacity: u64,
    }

    impl FixedReserve {
        fn count_at(&self, po: u8) -> u64 {
            self.bin_counts
                .get(usize::from(po))
                .copied()
                .unwrap_or_default()
        }
    }

    impl SwarmLocalStore for FixedReserve {
        fn put(&self, _chunk: CachedChunk) -> SwarmResult<()> {
            Ok(())
        }
        fn get(&self, _address: &ChunkAddress) -> SwarmResult<Option<CachedChunk>> {
            Ok(None)
        }
        fn contains(&self, _address: &ChunkAddress) -> bool {
            false
        }
        fn remove(&self, _address: &ChunkAddress) -> SwarmResult<()> {
            Ok(())
        }
    }

    impl ReserveStore for FixedReserve {
        fn storage_radius(&self) -> StorageRadius {
            self.radius
        }
        fn is_responsible_for(&self, _address: &ChunkAddress) -> bool {
            true
        }
        fn count(&self) -> SwarmResult<u64> {
            Ok(self.bin_counts.iter().sum())
        }
        fn capacity(&self) -> u64 {
            self.capacity
        }
        fn count_in(&self, po: ProximityOrder) -> SwarmResult<u64> {
            Ok(self.count_at(po.get()))
        }
        fn evict_furthest(&self) -> SwarmResult<Option<ChunkAddress>> {
            Ok(None)
        }
        fn evict_from_bin(&self, _bin: Bin, _max: u64) -> SwarmResult<u64> {
            Ok(0)
        }
        fn evict_batch(
            &self,
            _batch: BatchId,
            _up_to_bin: Option<Bin>,
            _max: u64,
        ) -> SwarmResult<u64> {
            Ok(0)
        }
    }

    impl SettableRadius for FixedReserve {
        fn set_storage_radius(&self, _radius: StorageRadius) {}
    }

    impl BinCursorStore for FixedReserve {
        // Cursor is a thousands-scaled bin index, distinct from the count.
        fn bin_cursor(&self, bin: Bin) -> SwarmResult<u64> {
            Ok(u64::from(bin.get()) * 1000)
        }
        fn scan_bin_from<'a>(
            &'a self,
            _bin: Bin,
            _start_seq: u64,
        ) -> SwarmResult<Box<dyn Iterator<Item = SwarmResult<BinScanItem>> + Send + 'a>> {
            Ok(Box::new(std::iter::empty()))
        }
    }

    fn service(
        radius: u8,
        bin_counts: Vec<u64>,
        capacity: u64,
    ) -> ReserveService<Arc<FixedReserve>> {
        ReserveService::new(Arc::new(FixedReserve {
            radius: StorageRadius::new(Bin::new(radius).expect("valid radius")),
            bin_counts,
            capacity,
        }))
    }

    /// The maximum proximity order, mirroring the service's own upper bound.
    const MAX_PO: u8 = ProximityOrder::MAX.get();

    #[tokio::test]
    async fn reserve_state_reports_radius_count_capacity() {
        // Total is the sum of the populated bins: 2 + 40 = 42.
        let svc = service(7, vec![0, 0, 2, 0, 0, 0, 0, 0, 40], 1 << 20);
        let resp = svc
            .get_reserve_state(Request::new(GetReserveStateRequest {}))
            .await
            .expect("state succeeds")
            .into_inner();

        assert_eq!(resp.storage_radius, 7);
        assert_eq!(resp.count, 42);
        assert_eq!(resp.capacity, 1 << 20);
    }

    #[tokio::test]
    async fn reserve_bins_cover_radius_through_max_po_inclusive() {
        let radius = 3u8;
        let svc = service(radius, vec![0; usize::from(MAX_PO) + 1], 1 << 20);
        let resp = svc
            .get_reserve_bins(Request::new(GetReserveBinsRequest {}))
            .await
            .expect("bins succeed")
            .into_inner();

        assert_eq!(resp.storage_radius, u32::from(radius));
        // Bins radius..=MAX_PO inclusive: the area of responsibility, never the
        // shallow complement below the radius.
        assert_eq!(resp.bins.len(), usize::from(MAX_PO - radius) + 1);
        for (idx, bin) in resp.bins.iter().enumerate() {
            let po = radius + idx as u8;
            assert_eq!(bin.proximity_order, u32::from(po));
            assert_eq!(bin.cursor, u64::from(po) * 1000);
        }
        // The shallow bins below the radius are absent.
        assert!(
            resp.bins
                .iter()
                .all(|b| b.proximity_order >= u32::from(radius))
        );
    }

    /// A non-zero radius with only deep bins populated: the populated bins appear
    /// and the per-bin counts sum to the in-responsibility total.
    #[tokio::test]
    async fn reserve_bins_report_populated_deep_bins_summing_to_count() {
        let radius = 8u8;
        // Populate a few deep bins (within the AoR) and a shallow bin (below the
        // radius) that must be excluded from the per-bin sum.
        let mut bin_counts = vec![0u64; usize::from(MAX_PO) + 1];
        bin_counts[2] = 99; // below the radius: shed, must not be reported.
        bin_counts[8] = 5; // at the radius.
        bin_counts[15] = 7;
        bin_counts[usize::from(MAX_PO)] = 3; // deepest bin.

        let svc = service(radius, bin_counts, 1 << 20);

        let state = svc
            .get_reserve_state(Request::new(GetReserveStateRequest {}))
            .await
            .expect("state succeeds")
            .into_inner();
        // GetReserveState.count is the whole reserve (including the stale shallow
        // bin, which the radius will shed): 99 + 5 + 7 + 3 = 114.
        assert_eq!(state.count, 114);

        let bins = svc
            .get_reserve_bins(Request::new(GetReserveBinsRequest {}))
            .await
            .expect("bins succeed")
            .into_inner();

        assert_eq!(bins.storage_radius, u32::from(radius));
        // Every reported bin is within the area of responsibility.
        assert!(
            bins.bins
                .iter()
                .all(|b| b.proximity_order >= u32::from(radius))
        );
        // The populated deep bins are present with their exact counts.
        let by_po = |po: u32| {
            bins.bins
                .iter()
                .find(|b| b.proximity_order == po)
                .map(|b| b.count)
                .unwrap_or_default()
        };
        assert_eq!(by_po(8), 5);
        assert_eq!(by_po(15), 7);
        assert_eq!(by_po(u32::from(MAX_PO)), 3);
        // The shallow bin below the radius is absent.
        assert!(!bins.bins.iter().any(|b| b.proximity_order == 2));

        // The per-bin counts within the AoR sum to count minus the shed shallow
        // bins, i.e. the in-responsibility reserve (5 + 7 + 3 = 15).
        let aor_sum: u64 = bins.bins.iter().map(|b| b.count).sum();
        assert_eq!(aor_sum, 15);
        assert_eq!(aor_sum, state.count - 99);
    }

    #[tokio::test]
    async fn reserve_bins_at_max_radius_yields_one_bin() {
        let svc = service(MAX_PO, vec![0; usize::from(MAX_PO) + 1], 1 << 20);
        let resp = svc
            .get_reserve_bins(Request::new(GetReserveBinsRequest {}))
            .await
            .expect("bins succeed")
            .into_inner();

        assert_eq!(resp.storage_radius, u32::from(MAX_PO));
        // At the deepest radius the AoR is the single bin MAX_PO.
        assert_eq!(resp.bins.len(), 1);
        assert_eq!(resp.bins[0].proximity_order, u32::from(MAX_PO));
    }
}
