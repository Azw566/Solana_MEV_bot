use std::collections::HashMap;
use log::{info, error};
use rust_socketio::asynchronous::Client;

use crate::markets::meteora::simulate_route_meteora;
use crate::markets::{orca_whirpools::simulate_route_orca_whirpools, raydium::simulate_route_raydium, types::{DexLabel, Market}};
use super::types::{SwapPath, SwapRouteSimulation, TokenInfos};

pub async fn simulate_path(
    simulation_amount: u64, 
    path: SwapPath, 
    markets: &HashMap<String, Market>, // Référence vers la HashMap globale
    tokens_infos: HashMap<String, TokenInfos>, 
    mut route_simulation: HashMap<Vec<u32>, Vec<SwapRouteSimulation>>
) -> (HashMap<Vec<u32>, Vec<SwapRouteSimulation>>, Vec<SwapRouteSimulation>, f64) {
    
    let decimals = 9;
    let mut amount_in = simulation_amount;
    let amount_begin = amount_in;
    let mut swap_simulation_result: Vec<SwapRouteSimulation> = Vec::new();
    
    for (i, route) in path.paths.iter().enumerate() {
        
        // --- OPTIMISATION : Accès direct par clé ---
        let market = markets.get(&route.pool_address);

        if market.is_none() {
            error!("Market {} introuvable dans le cache.", route.pool_address);
            return (route_simulation, Vec::new(), 0.0);
        }
        let current_market = market.unwrap();

        // Gestion du cache de simulation (No simulation si déjà calculé)
        match path.hops {
            1 => {
                if i == 0 && route_simulation.contains_key(&vec![path.id_paths[i]]) {
                    let swap_sim = route_simulation.get(&vec![path.id_paths[i]]).unwrap();
                    amount_in = swap_sim[0].estimated_amount_out.parse().unwrap_or(0.0) as u64;
                    swap_simulation_result.push(swap_sim[0].clone());
                    continue;
                }
            }
            2 => {
                if i == 0 && route_simulation.contains_key(&vec![path.id_paths[i]]) {
                    let swap_sim = route_simulation.get(&vec![path.id_paths[i]]).unwrap();
                    amount_in = swap_sim[0].estimated_amount_out.parse().unwrap_or(0.0) as u64;
                    swap_simulation_result.push(swap_sim[0].clone());
                    continue;
                }
                if i == 1 && route_simulation.contains_key(&vec![path.id_paths[i - 1], path.id_paths[i]]) {
                    let swap_sim = route_simulation.get(&vec![path.id_paths[i - 1], path.id_paths[i]]).unwrap();
                    amount_in = swap_sim[1].estimated_amount_out.parse().unwrap_or(0.0) as u64;
                    swap_simulation_result.push(swap_sim[1].clone());
                    continue;
                }
            }
            _ => {}
        }

        // --- SIMULATION PAR DEX ---
        let sim_result = match route.dex {
            DexLabel::ORCA_WHIRLPOOLS => {
                simulate_route_orca_whirpools(true, amount_in, route.clone(), current_market, tokens_infos.clone()).await
            },
            DexLabel::RAYDIUM => {
                simulate_route_raydium(true, amount_in, route.clone(), current_market, tokens_infos.clone()).await
            },
            DexLabel::METEORA => {
                simulate_route_meteora(true, amount_in, route.clone(), current_market, tokens_infos.clone()).await
            },
            _ => {
                error!("DEX {:?} non supporté pour la simulation rapide", route.dex);
                return (route_simulation, Vec::new(), 0.0);
            }
        };

        match sim_result {
            Ok((amount_out, min_amount_out)) => {
                let swap_sim = SwapRouteSimulation {
                    id_route: route.id,
                    pool_address: route.pool_address.clone(),
                    dex_label: route.dex.clone(),
                    token_0to1: route.token_0to1,
                    token_in: route.tokenIn.clone(),
                    token_out: route.tokenOut.clone(),
                    amount_in,
                    estimated_amount_out: amount_out.clone(),
                    estimated_min_amount_out: min_amount_out,
                };

                // Mise à jour du cache local de la route
                if i == 0 {
                    route_simulation.entry(vec![route.id]).or_insert(vec![swap_sim.clone()]);
                } else if i == 1 && path.hops == 2 {
                    if let Some(prev) = route_simulation.get(&vec![path.id_paths[i - 1]]) {
                        route_simulation.insert(vec![path.id_paths[i - 1], path.id_paths[i]], vec![prev[0].clone(), swap_sim.clone()]);
                    }
                }

                swap_simulation_result.push(swap_sim);
                amount_in = amount_out.parse().unwrap_or(0.0) as u64;
            }
            Err(e) => {
                error!("Erreur simulation sur {:?}: {:?}", route.pool_address, e);
                return (route_simulation, Vec::new(), 0.0);
            }
        }
    }

    let difference = amount_in as f64 - amount_begin as f64;
    if difference > 0.0 {
        info!(" OPPORTUNITÉ: +{} SOL sur trajet {:?}", difference / 10f64.powi(9), path.id_paths);
    }

    (route_simulation, swap_simulation_result, difference)
}