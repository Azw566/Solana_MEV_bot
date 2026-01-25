use crate::markets::raydium::AmmInfo;
use crate::markets::types::{Market, TokenInfos};
use crate::arbitrage::types::{SwapPath, TokenInArb, SwapRouteSimulation};
use crate::arbitrage::calc_arb::calculate_arb; // Assure-toi du chemin exact
use crate::arbitrage::simulate::simulate_path; // Assure-toi du chemin exact
use borsh::BorshDeserialize;
use yellowstone_grpc_client::GeyserServiceClient;
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SubscribeRequest, SubscribeRequestFilterAccounts,
};
use std::collections::HashMap;
use futures::stream::StreamExt;
use log::{info, error};

pub async fn run_grpc_vortex(
    endpoint: String, 
    x_token: String, 
    mut global_markets: HashMap<String, Market>,
    tokens: Vec<TokenInArb>,
    tokens_infos: HashMap<String, TokenInfos>
) -> anyhow::Result<()> {
    // 1. Initialisation des chemins (Une seule fois au démarrage)
    // On génère tous les chemins possibles
    let (sorted_markets, all_paths) = calculate_arb(true, true, global_markets.clone(), tokens.clone());
    
    // 2. Création de l'index pour la performance
    // Cet index lie "Adresse du Pool" -> "Liste des chemins qui l'utilisent"
    let mut paths_by_pool: HashMap<String, Vec<SwapPath>> = HashMap::new();
    for path in all_paths {
        for route in &path.paths {
            paths_by_pool
                .entry(route.pool_address.clone())
                .or_insert_with(Vec::new)
                .push(path.clone());
        }
    }

    let mut route_cache: HashMap<Vec<u32>, Vec<SwapRouteSimulation>> = HashMap::new();
    let mut client = GeyserServiceClient::connect(endpoint, Some(x_token), None).await?;

    let mut accounts = HashMap::new();
    accounts.insert(
        "raydium_filter".to_string(),
        SubscribeRequestFilterAccounts {
            owner: vec!["675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string()],
            account: vec![],
            nonempty: Some(true),
        },
    );

    let (mut subscribe_tx, mut stream) = client.subscribe().await?;
    subscribe_tx.send(SubscribeRequest { accounts, ..Default::default() }).await?;

    info!(" gRPC Vortex branché sur Raydium. {} pools indexés.", paths_by_pool.len());

    while let Some(message) = stream.next().await {
        let message = message?;
        if let Some(UpdateOneof::Account(acc)) = message.update_oneof {
            let account_info = acc.account.unwrap();
            let pubkey_str = bs58::encode(account_info.pubkey).into_string();

            // Si le pool qui a bougé est dans notre liste surveillée
            if let Some(market) = global_markets.get_mut(&pubkey_str) {
                if let Ok(_) = AmmInfo::try_from_slice(&account_info.data) {
                    
                    // Mise à jour de la donnée brute pour les futures simulations
                    market.account_data = Some(account_info.data);

                    // 3. Déclenchement chirurgical de la simulation
                    if let Some(impacted_paths) = paths_by_pool.get(&pubkey_str) {
                        for path in impacted_paths {
                            // On transforme la Map en Vec pour simulate_path (nécessaire selon ta signature)
                            // plus besoin de let markets_vec: Vec<Market> = global_markets.values().cloned().collect();

                            let (new_cache, _results, profit) = simulate_path(
                                1_000_000_000, // 1 SOL (ajuste selon tes besoins)
                                path.clone(),
                                &global_markets, // Passage par référence
                                tokens_infos.clone(),
                                route_cache.clone()
                            ).await;

                            route_cache = new_cache;

                            if profit > 0.0 {
                                info!(" PROFIT DÉTECTÉ sur {}: {} SOL", pubkey_str, profit / 10.0_f64.powi(9));
                                // Ici, tu peux appeler simulate_path_precision ou envoyer la TX
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}