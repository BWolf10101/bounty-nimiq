use std::sync::Arc;

use nimiq_blockchain::Blockchain;
use nimiq_blockchain_interface::AbstractBlockchain;
use nimiq_blockchain_proxy::BlockchainProxy;
use parking_lot::RwLock;
use prometheus_client::registry::Registry;

use crate::NumericClosureMetric;

pub struct BlockMetrics {}

impl BlockMetrics {
    pub fn register(registry: &mut Registry, blockchain_proxy: BlockchainProxy) {
        BlockMetrics::register_chain(registry, blockchain_proxy.clone());
        if let BlockchainProxy::Full(blockchain) = blockchain_proxy {
            BlockMetrics::register_accounts_trie(registry, blockchain.clone());
            BlockMetrics::register_staking(registry, blockchain.clone());
            let sub_registry = registry.sub_registry_with_prefix("blockchain");
            blockchain.read().metrics().register(sub_registry);
        }
    }

    fn register_staking(registry: &mut Registry, blockchain: Arc<RwLock<Blockchain>>) {
        let sub_registry = registry.sub_registry_with_prefix("staking");

        let bc = blockchain;
        let closure = NumericClosureMetric::new_gauge(Box::new(move || {
            bc.read()
                .get_staking_contract_if_complete(None)
                .map(|contract| contract.active_validators.len() as i64)
                .unwrap_or(0)
        }));
        sub_registry.register("active_validators", "Number of active validators", closure);
    }

    fn register_accounts_trie(registry: &mut Registry, blockchain: Arc<RwLock<Blockchain>>) {
        let sub_registry = registry.sub_registry_with_prefix("accounts_trie");

        let bc = blockchain.clone();
        let closure = NumericClosureMetric::new_gauge(Box::new(move || {
            bc.read().state.accounts.size() as i64
        }));
        sub_registry.register("accounts", "Number of accounts", closure);

        let closure = NumericClosureMetric::new_gauge(Box::new(move || {
            blockchain.read().state.accounts.num_branches() as i64
        }));
        sub_registry.register("num_branches", "Number of branch nodes", closure);
    }

    fn register_chain(registry: &mut Registry, blockchain: BlockchainProxy) {
        let sub_registry = registry.sub_registry_with_prefix("blockchain");

        let bc = blockchain.clone();
        let closure =
            NumericClosureMetric::new_gauge(Box::new(move || bc.read().block_number() as i64));
        sub_registry.register("block_number", "Number of latest block", closure);

        let bc = blockchain.clone();
        let closure =
            NumericClosureMetric::new_gauge(Box::new(move || bc.read().batch_number() as i64));
        sub_registry.register("batch_number", "Number of latest batch", closure);

        let bc = blockchain.clone();
        let closure =
            NumericClosureMetric::new_gauge(Box::new(move || bc.read().epoch_number() as i64));
        sub_registry.register("epoch_number", "Number of latest epoch", closure);

        let bc = blockchain.clone();
        let closure =
            NumericClosureMetric::new_gauge(Box::new(move || bc.read().timestamp() as i64));
        sub_registry.register("timestamp", "Timestamp of latest block", closure);

        let closure = NumericClosureMetric::new_gauge(Box::new(move || {
            blockchain
                .read()
                .current_validators()
                .map(|validators| validators.num_validators())
                .unwrap_or(0) as i64
        }));
        sub_registry.register(
            "elected_validators",
            "Number of elected validators",
            closure,
        );
    }
}
