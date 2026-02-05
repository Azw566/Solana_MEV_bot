use std::collections::{HashMap, HashSet};
use log::{info, warn};

use crate::arbitrage::types::{Route, SwapPath};

/// Directed edge in the arbitrage graph representing a single swap route.
#[derive(Debug, Clone)]
pub struct ArbEdge {
    /// Index of the source token (node) in the graph
    pub from: usize,
    /// Index of the destination token (node) in the graph
    pub to: usize,
    /// Edge weight = -ln(exchange_rate).
    /// A negative weight means rate > 1 (output exceeds input after fees).
    /// Summing weights along a path equals -ln(product of rates).
    /// A negative-weight cycle <==> product of rates > 1 <==> arbitrage profit.
    pub weight: f64,
    /// The original Route data this edge was built from
    pub route: Route,
}

/// Weighted directed graph for Bellman-Ford-based arbitrage detection.
///
/// **Nodes** are tokens (identified by mint address).
/// **Edges** are swap routes (one per direction per pool).
/// **Weights** are `-ln(exchange_rate)` so that finding a negative-weight
/// cycle is equivalent to finding a circular trade whose combined exchange
/// rate exceeds 1 (i.e. a profitable arbitrage).
///
/// # How it works
///
/// 1. Simulate every route with a reference input amount to obtain exchange rates.
/// 2. Build this graph with `ArbGraph::new(routes, exchange_rates)`.
/// 3. Call `find_arbitrage_cycles(source_token, max_hops)` to detect profitable
///    round-trips starting and ending at the base token.
/// 4. Feed the resulting `SwapPath` candidates into the existing `simulate_path`
///    pipeline for full on-chain validation.
#[derive(Debug)]
pub struct ArbGraph {
    /// Total number of unique tokens (nodes)
    pub num_nodes: usize,
    /// All directed edges (swap routes with computed weights)
    pub edges: Vec<ArbEdge>,
    /// Maps token mint address -> node index
    pub token_to_index: HashMap<String, usize>,
    /// Maps node index -> token mint address
    pub index_to_token: Vec<String>,
}

impl ArbGraph {
    /// Build the arbitrage graph from routes and pre-computed exchange rates.
    ///
    /// # Arguments
    /// * `routes`          - All directional swap routes (from `compute_routes`)
    /// * `exchange_rates`  - Map of `route.id -> (amount_out / amount_in)` obtained
    ///                       by simulating each route with a reference input amount.
    ///                       Routes missing from this map are excluded from the graph.
    pub fn new(routes: &[Route], exchange_rates: &HashMap<u32, f64>) -> Self {
        let mut token_to_index: HashMap<String, usize> = HashMap::new();
        let mut index_to_token: Vec<String> = Vec::new();

        // Assign a unique index to every token that appears in the routes
        for route in routes {
            for token in [&route.tokenIn, &route.tokenOut] {
                if !token_to_index.contains_key(token) {
                    let idx = index_to_token.len();
                    token_to_index.insert(token.clone(), idx);
                    index_to_token.push(token.clone());
                }
            }
        }

        let num_nodes = index_to_token.len();
        let mut edges = Vec::new();

        for route in routes {
            if let Some(&rate) = exchange_rates.get(&route.id) {
                if rate <= 0.0 {
                    continue;
                }
                let from = token_to_index[&route.tokenIn];
                let to = token_to_index[&route.tokenOut];

                edges.push(ArbEdge {
                    from,
                    to,
                    weight: -(rate.ln()),
                    route: route.clone(),
                });
            }
        }

        info!(
            "Arbitrage graph built: {} tokens (nodes), {} routes (edges)",
            num_nodes,
            edges.len()
        );

        ArbGraph {
            num_nodes,
            edges,
            token_to_index,
            index_to_token,
        }
    }

    /// Run Bellman-Ford from `source_token` and collect every negative-weight
    /// cycle that starts **and** ends at the source (arbitrage round-trips).
    ///
    /// # Arguments
    /// * `source_token` - Mint address of the starting token (e.g. SOL)
    /// * `max_hops`     - Maximum number of intermediate hops in a cycle.
    ///   - `1` finds SOL -> X -> SOL           (2 edges, 1 intermediate token)
    ///   - `2` finds SOL -> X -> Y -> SOL      (3 edges, 2 intermediate tokens)
    ///   - `3` finds SOL -> X -> Y -> Z -> SOL (4 edges, 3 intermediate tokens)
    ///
    /// # Returns
    /// `SwapPath` candidates sorted by estimated profitability (best first),
    /// ready to be validated by `simulate_path`.
    pub fn find_arbitrage_cycles(
        &self,
        source_token: &str,
        max_hops: usize,
    ) -> Vec<SwapPath> {
        let source = match self.token_to_index.get(source_token) {
            Some(&idx) => idx,
            None => {
                warn!("Source token {} not found in graph", source_token);
                return Vec::new();
            }
        };

        let n = self.num_nodes;
        if n == 0 {
            return Vec::new();
        }

        let inf = f64::MAX / 2.0;

        // dist[v] = weight of the shortest path from source to v found so far
        let mut dist: Vec<f64> = vec![inf; n];
        // predecessor[v] = index into self.edges of the edge used to arrive at v
        let mut predecessor: Vec<Option<usize>> = vec![None; n];

        dist[source] = 0.0;

        let mut found_cycles: Vec<(f64, SwapPath)> = Vec::new();
        let mut seen_signatures: HashSet<Vec<u32>> = HashSet::new();

        // Run up to `max_hops` relaxation passes.
        // After pass k (0-indexed), dist[v] holds the best path from source
        // using at most k+1 edges.  Probing closing edges back to source after
        // pass k yields cycles of at most k+2 edges = k+1 hops.
        //
        // So for max_hops = H we run H passes (0..H) and find cycles up to H+1
        // edges = H hops.  Since hops = (edges - 1), a cycle found after pass k
        // has at most (k+2 - 1) = k+1 hops. With k going up to H-1 the max is H.
        let iterations = max_hops.min(n);

        for iter in 0..iterations {
            // Snapshot distances from the previous iteration so that
            // relaxation within this pass is order-independent (synchronous BF).
            let prev_dist = dist.clone();
            let mut updated = false;

            for (eidx, edge) in self.edges.iter().enumerate() {
                if prev_dist[edge.from] < inf
                    && prev_dist[edge.from] + edge.weight < dist[edge.to] - 1e-12
                {
                    dist[edge.to] = prev_dist[edge.from] + edge.weight;
                    predecessor[edge.to] = Some(eidx);
                    updated = true;
                }
            }

            // After pass 0 the shortest 1-edge paths are known.
            // A closing edge back to source produces a 2-edge cycle (1 hop).
            // We check after every pass, including pass 0.
            for (eidx, edge) in self.edges.iter().enumerate() {
                if edge.to == source && dist[edge.from] < inf {
                    let cycle_weight = dist[edge.from] + edge.weight;
                    if cycle_weight < -1e-10 {
                        if let Some(path) =
                            self.reconstruct_cycle(source, eidx, &predecessor)
                        {
                            if !seen_signatures.contains(&path.id_paths) {
                                seen_signatures.insert(path.id_paths.clone());
                                // profitability = -cycle_weight (higher is better)
                                found_cycles.push((-cycle_weight, path));
                            }
                        }
                    }
                }
            }

            if !updated {
                break;
            }
        }

        // Sort by estimated profitability (most profitable first)
        found_cycles.sort_by(|a, b| {
            b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
        });

        info!(
            "Bellman-Ford: {} arbitrage candidates detected (max {} hops)",
            found_cycles.len(),
            max_hops
        );

        found_cycles.into_iter().map(|(_, p)| p).collect()
    }

    /// Trace predecessor edges backwards from the node before the closing edge
    /// all the way back to `source`, building the complete cycle path.
    ///
    /// Returns `None` when the path is invalid (visits a node twice, or
    /// reuses a pool address).
    fn reconstruct_cycle(
        &self,
        source: usize,
        closing_edge_idx: usize,
        predecessor: &[Option<usize>],
    ) -> Option<SwapPath> {
        let closing_edge = &self.edges[closing_edge_idx];

        // Walk backwards collecting edge indices
        let mut edge_indices: Vec<usize> = vec![closing_edge_idx];
        let mut current = closing_edge.from;
        let mut visited_nodes: HashSet<usize> = HashSet::new();
        visited_nodes.insert(source);

        while current != source {
            if visited_nodes.contains(&current) {
                return None; // Internal loop not returning to source
            }
            visited_nodes.insert(current);

            match predecessor[current] {
                Some(eidx) => {
                    edge_indices.push(eidx);
                    current = self.edges[eidx].from;
                }
                None => return None, // No path back to source
            }
        }

        // Reverse to get source-first order
        edge_indices.reverse();

        // Reject cycles that reuse the same pool (a pool can only be used once per cycle)
        let pools: HashSet<&str> = edge_indices
            .iter()
            .map(|&i| self.edges[i].route.pool_address.as_str())
            .collect();
        if pools.len() != edge_indices.len() {
            return None;
        }

        let routes: Vec<Route> = edge_indices
            .iter()
            .map(|&i| self.edges[i].route.clone())
            .collect();
        let id_paths: Vec<u32> = routes.iter().map(|r| r.id).collect();

        // Convention: hops = number of intermediate tokens = routes.len() - 1
        // 2 routes (SOL->X->SOL) = 1 hop
        // 3 routes (SOL->X->Y->SOL) = 2 hops
        // 4 routes (SOL->X->Y->Z->SOL) = 3 hops
        let hops = (routes.len() - 1) as u8;

        Some(SwapPath {
            hops,
            paths: routes,
            id_paths,
        })
    }

    /// Update the weight of a specific edge given a new (more accurate) exchange rate.
    /// Useful after re-simulating a route with a different amount or fresh pool state.
    pub fn update_edge_weight(&mut self, route_id: u32, new_exchange_rate: f64) {
        if new_exchange_rate <= 0.0 {
            return;
        }
        let new_weight = -(new_exchange_rate.ln());
        for edge in &mut self.edges {
            if edge.route.id == route_id {
                edge.weight = new_weight;
                break;
            }
        }
    }

    /// Return a summary of the graph for logging / debugging.
    pub fn summary(&self) -> String {
        let mut dex_counts: HashMap<String, usize> = HashMap::new();
        for edge in &self.edges {
            let label = format!("{:?}", edge.route.dex);
            *dex_counts.entry(label).or_insert(0) += 1;
        }
        let dex_info: Vec<String> = dex_counts
            .iter()
            .map(|(k, v)| format!("{}: {} edges", k, v))
            .collect();
        format!(
            "ArbGraph {{ {} nodes, {} edges | {} }}",
            self.num_nodes,
            self.edges.len(),
            dex_info.join(", ")
        )
    }
}
