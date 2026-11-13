// SPDX-License-Identifier: GPL-3.0-or-later
//! A static 3-D k-d tree over points: k nearest neighbours and radius queries.

use glam::DVec3;
use std::collections::BinaryHeap;

/// A k-nearest candidate, ordered by distance (the heap keeps the farthest on top).
struct Candidate {
    distance: f64,
    index: usize,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.distance.total_cmp(&other.distance).is_eq()
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.distance.total_cmp(&other.distance)
    }
}

/// A k-d tree over a fixed set of points.
pub struct KdTree {
    points: Vec<DVec3>,
    /// Point indices in tree order; each node is the median of its range.
    order: Vec<usize>,
}

impl KdTree {
    /// Build the tree over these points.
    pub fn new(points: Vec<DVec3>) -> Self {
        let mut order: Vec<usize> = (0..points.len()).collect();
        build(&points, &mut order, 0);
        Self { points, order }
    }

    /// The number of points.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether the tree holds no points.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// The point of an index, as given to [`Self::new`].
    pub fn point(&self, index: usize) -> DVec3 {
        self.points[index]
    }

    /// The k nearest points to q, nearest first: (index, distance).
    pub fn nearest(&self, q: DVec3, k: usize) -> Vec<(usize, f64)> {
        let mut best: BinaryHeap<Candidate> = BinaryHeap::with_capacity(k + 1);
        self.knn(&self.order, 0, q, k, &mut best);
        let mut out: Vec<(usize, f64)> = best.into_iter().map(|c| (c.index, c.distance)).collect();
        out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        out
    }

    fn knn(
        &self,
        range: &[usize],
        depth: usize,
        q: DVec3,
        k: usize,
        best: &mut BinaryHeap<Candidate>,
    ) {
        if range.is_empty() || k == 0 {
            return;
        }
        let mid = range.len() / 2;
        let index = range[mid];
        let p = self.points[index];
        let d = p.distance(q);
        if best.len() < k {
            best.push(Candidate { distance: d, index });
        } else if best.peek().is_some_and(|w| d < w.distance) {
            best.pop();
            best.push(Candidate { distance: d, index });
        }
        let axis = depth % 3;
        let diff = q[axis] - p[axis];
        let (near, far) = if diff < 0. {
            (&range[..mid], &range[mid + 1..])
        } else {
            (&range[mid + 1..], &range[..mid])
        };
        self.knn(near, depth + 1, q, k, best);
        if best.len() < k || best.peek().is_some_and(|w| diff.abs() < w.distance) {
            self.knn(far, depth + 1, q, k, best);
        }
    }

    /// Every point within r of q (unordered).
    pub fn within(&self, q: DVec3, r: f64) -> Vec<usize> {
        let mut out = Vec::new();
        self.radius(&self.order, 0, q, r, &mut out);
        out
    }

    fn radius(&self, range: &[usize], depth: usize, q: DVec3, r: f64, out: &mut Vec<usize>) {
        if range.is_empty() {
            return;
        }
        let mid = range.len() / 2;
        let index = range[mid];
        let p = self.points[index];
        if p.distance(q) <= r {
            out.push(index);
        }
        let axis = depth % 3;
        let diff = q[axis] - p[axis];
        if diff - r <= 0. {
            self.radius(&range[..mid], depth + 1, q, r, out);
        }
        if diff + r >= 0. {
            self.radius(&range[mid + 1..], depth + 1, q, r, out);
        }
    }
}

fn build(points: &[DVec3], range: &mut [usize], depth: usize) {
    if range.len() <= 1 {
        return;
    }
    let axis = depth % 3;
    let mid = range.len() / 2;
    range.select_nth_unstable_by(mid, |&a, &b| points[a][axis].total_cmp(&points[b][axis]));
    let (left, right) = range.split_at_mut(mid);
    build(points, left, depth + 1);
    build(points, &mut right[1..], depth + 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_and_radius_match_brute_force() {
        let points: Vec<DVec3> = (0..500)
            .map(|k| {
                let x = k as f64;
                DVec3::new(
                    (x * 0.37).sin() * 5.,
                    (x * 0.11).cos() * 3.,
                    (x * 0.07).sin() * 7.,
                )
            })
            .collect();
        let tree = KdTree::new(points.clone());
        let q = DVec3::new(0.3, -0.2, 1.);
        let mut brute: Vec<(usize, f64)> = points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.distance(q)))
            .collect();
        brute.sort_by(|a, b| a.1.total_cmp(&b.1));
        let found = tree.nearest(q, 7);
        assert_eq!(
            found.iter().map(|x| x.0).collect::<Vec<_>>(),
            brute[..7].iter().map(|x| x.0).collect::<Vec<_>>()
        );
        let mut within = tree.within(q, 2.);
        within.sort();
        let mut expected: Vec<usize> = brute.iter().filter(|x| x.1 <= 2.).map(|x| x.0).collect();
        expected.sort();
        assert_eq!(within, expected);
    }
}
