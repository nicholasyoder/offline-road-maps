//! Travel times between every pair of a trip's stops (phase 4 step 4.1), by bucket
//! many-to-many over the hierarchy (Knopp, Sanders, Schultes, Schulz and Wagner 2007). A
//! backward search up from each stop leaves `(stop, distance)` in a bucket at every node
//! it settles and does not stall; then a forward search up from each stop reads the
//! buckets of the nodes it settles. That is two searches a stop, with no paths unpacked:
//! no geometry and no directions.
//!
//! The searches start from the seeds a leg's search does, on the edges it may use, so
//! each entry is the weight [`Routing::leg_avoiding`] gives that leg, the restricted-road
//! penalty included, and an optimized trip's legs add up to the matrix's total. Pairs on
//! one segment (which a leg may drive along it, without the graph), and pairs with no
//! route avoiding all that was asked, are routed as legs instead.

use std::io;
use std::time::{Duration, Instant};

use super::order::{self, Ends, Matrix, NONE};
use super::query::stopped;
use super::{AVOID, Routing, Snap, Stop};

/// Stops reordered for the quickest trip.
#[derive(Clone, Debug)]
pub struct Optimized {
    /// The new order, as indices of the stops given.
    pub order: Vec<usize>,
    /// The matrix's weight of the trip in the order given, and in the new one (back to
    /// the first stop for a round trip); `None` if a leg has no route.
    pub before: Option<u64>,
    pub after: Option<u64>,
    pub matrix: Matrix,
    /// Time spent on the matrix, and on the order.
    pub matrix_time: Duration,
    pub order_time: Duration,
    /// Nodes the matrix's searches settled.
    pub settled: usize,
}

impl Routing {
    /// The weight of the leg [`Self::leg_avoiding`] routes between every pair of `snaps`
    /// ([`NONE`] where there is no route), and the nodes the searches settled. `None` once
    /// `stop` returns true; it is asked between searches, and every few nodes settled.
    pub fn matrix_until(
        &self,
        snaps: &[Snap],
        avoid: u8,
        stop: &dyn Fn() -> bool,
    ) -> io::Result<Option<(Matrix, usize)>> {
        match self.matrix_inner(snaps, avoid & AVOID, stop) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(None),
            r => r.map(Some),
        }
    }

    fn matrix_inner(
        &self,
        snaps: &[Snap],
        avoid: u8,
        stop: &dyn Fn() -> bool,
    ) -> io::Result<(Matrix, usize)> {
        let n = snaps.len();
        let mut m = Matrix::new(n);
        let mut settled = 0;
        let segments = snaps
            .iter()
            .map(|s| self.segment(s.segment))
            .collect::<io::Result<Vec<_>>>()?;
        // `(node, stop, distance from the node to the stop)`, by node.
        let mut buckets: Vec<(u32, u32, u32)> = Vec::new();
        for (j, (snap, seg)) in snaps.iter().zip(&segments).enumerate() {
            let (targets, _) = self.targets(snap, seg)?;
            settled += self.upward(&targets, false, avoid, stop, &mut |v, d| {
                buckets.push((v, j as u32, d));
            })?;
        }
        buckets.sort_unstable();
        let mut row = vec![NONE; n];
        for (i, (snap, seg)) in snaps.iter().zip(&segments).enumerate() {
            row.fill(NONE);
            let sources = Self::sources(snap, seg);
            settled += self.upward(&sources, true, avoid, stop, &mut |v, d| {
                let at = buckets.partition_point(|b| b.0 < v);
                for &(_, j, dj) in buckets[at..].iter().take_while(|b| b.0 == v) {
                    let w = &mut row[j as usize];
                    *w = (*w).min(d + dj);
                }
            })?;
            for (j, &w) in row.iter().enumerate().filter(|&(j, _)| j != i) {
                m.set(i, j, w);
            }
        }
        for i in 0..n {
            for j in (0..n).filter(|&j| j != i) {
                let same = snaps[i].segment == snaps[j].segment;
                if same || (m.get(i, j) == NONE && avoid != 0) {
                    if stop() {
                        return Err(stopped());
                    }
                    let leg = self.leg_avoiding_until(&snaps[i], &snaps[j], avoid, stop)?;
                    if let Some(l) = &leg {
                        settled += l.settled;
                    }
                    m.set(i, j, leg.map_or(NONE, |l| l.weight));
                }
            }
        }
        Ok((m, settled))
    }

    /// The quickest order of `stops` ([`order::order`]) over their matrix. An error if a
    /// stop has no road near it; `None` once `stop` returns true.
    pub fn optimize_until(
        &self,
        stops: &[Stop],
        avoid: u8,
        ends: Ends,
        stop: &dyn Fn() -> bool,
    ) -> io::Result<Option<Optimized>> {
        let t = Instant::now();
        let mut snaps = Vec::with_capacity(stops.len());
        for (i, s) in stops.iter().enumerate() {
            if stop() {
                return Ok(None);
            }
            let snap = self.snap(s)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("stop {} has no road within reach", i + 1),
                )
            })?;
            snaps.push(snap);
        }
        let Some((matrix, settled)) = self.matrix_until(&snaps, avoid, stop)? else {
            return Ok(None);
        };
        let matrix_time = t.elapsed();
        let t = Instant::now();
        let Some(order) = order::order(&matrix, ends, stop) else {
            return Ok(None);
        };
        let identity: Vec<usize> = (0..stops.len()).collect();
        Ok(Some(Optimized {
            before: matrix.total(&identity, ends.round_trip),
            after: matrix.total(&order, ends.round_trip),
            order,
            matrix,
            matrix_time,
            order_time: t.elapsed(),
            settled,
        }))
    }
}
