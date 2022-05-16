use crate::{models, Balances};
use cached::Cached;
use std::str::FromStr;

use crate::models::balance_changes::BalanceChange;
use crate::models::PrintEnum;
use anyhow::Context;
use bigdecimal::BigDecimal;
use futures::future::try_join_all;
use futures::SinkExt;
use near_indexer_primitives::views::StateChangeCauseView;
use num_traits::Zero;

pub(crate) async fn store_balance_changes(
    pool: &sqlx::Pool<sqlx::Postgres>,
    shards: &[near_indexer_primitives::IndexerShard],
    block_header: &near_indexer_primitives::views::BlockHeaderView,
    balances_cache: crate::BalancesCache,
) -> anyhow::Result<()> {
    let futures = shards
        .iter()
        .map(|shard| store_changes_for_chunk(pool, shard, block_header, balances_cache.clone()));

    try_join_all(futures).await.map(|_| ())
}

async fn store_changes_for_chunk(
    pool: &sqlx::Pool<sqlx::Postgres>,
    shard: &near_indexer_primitives::IndexerShard,
    block_header: &near_indexer_primitives::views::BlockHeaderView,
    balances_cache: crate::BalancesCache,
) -> anyhow::Result<()> {
    let mut changes: Vec<BalanceChange> = vec![];
    changes.extend(
        store_validator_accounts_update_for_chunk(
            &shard.state_changes,
            block_header,
            shard.shard_id,
            balances_cache.clone(),
        )
        .await?,
    );
    match shard.chunk.as_ref().map(|chunk| &chunk.transactions) {
        None => {}
        Some(x) => changes.extend(
            store_transaction_execution_outcomes_for_chunk(
                x,
                block_header,
                shard.shard_id,
                balances_cache.clone(),
            )
            .await?,
        ),
    }

    changes.iter_mut().enumerate().for_each(|(i, mut change)| {
        change.index_in_chunk = i as i32;
    });
    models::chunked_insert(pool, &changes, 10).await?;
    Ok(())
}

async fn store_validator_accounts_update_for_chunk(
    state_changes: &near_indexer_primitives::views::StateChangesView,
    block_header: &near_indexer_primitives::views::BlockHeaderView,
    shard_id: near_indexer_primitives::types::ShardId,
    balances_cache: crate::BalancesCache,
) -> anyhow::Result<Vec<BalanceChange>> {
    let mut result: Vec<BalanceChange> = vec![];
    for state_change_with_cause in state_changes {
        let near_indexer_primitives::views::StateChangeWithCauseView { cause, value } =
            state_change_with_cause;

        let (account_id, account): (String, &near_indexer_primitives::views::AccountView) =
            match value {
                near_indexer_primitives::views::StateChangeValueView::AccountUpdate {
                    account_id,
                    account,
                } => (account_id.to_string(), account),
                // other values should be fully covered in execution outcomes
                // or they do not provide balance changes
                _ => continue,
            };

        match cause {
            StateChangeCauseView::NotWritableToDisk
            | StateChangeCauseView::InitialState
            | StateChangeCauseView::UpdatedDelayedReceipts
            | StateChangeCauseView::Migration
            | StateChangeCauseView::Resharding => {
                panic!("let's debug it");
            }
            StateChangeCauseView::ValidatorAccountsUpdate => {
                let prev_balances: Balances = get_previous_balance(
                    &account_id.parse().unwrap(),
                    balances_cache.clone(),
                    block_header.prev_hash,
                )
                .await?;

                let delta_liquid_amount: i128 =
                    (account.amount as i128) - (prev_balances.0 as i128);
                let delta_locked_amount: i128 =
                    (account.locked as i128) - (prev_balances.1 as i128);

                set_new_balances(
                    account_id.parse().unwrap(),
                    (account.amount, account.locked),
                    balances_cache.clone(),
                ).await;

                result.push(BalanceChange {
                    block_timestamp: block_header.timestamp.into(),
                    receipt_id: None,
                    transaction_hash: None,
                    affected_account_id: account_id,
                    involved_account_id: None,
                    direction: "NONE".to_string(),
                    cause: cause.print().to_string(),
                    delta_liquid_amount: BigDecimal::from_str(&delta_liquid_amount.to_string())
                        .unwrap(),
                    absolute_liquid_amount: BigDecimal::from_str(&account.amount.to_string())
                        .unwrap(),
                    delta_locked_amount: BigDecimal::from_str(&delta_locked_amount.to_string())
                        .unwrap(),
                    absolute_locked_amount: BigDecimal::from_str(&account.locked.to_string())
                        .unwrap(),
                    shard_id: shard_id as i32,
                    // will enumerate later
                    index_in_chunk: 0,
                });
            }
            _ => continue,
        }
    }
    Ok(result)
}

async fn store_transaction_execution_outcomes_for_chunk(
    transactions: &[near_indexer_primitives::IndexerTransactionWithOutcome],
    block_header: &near_indexer_primitives::views::BlockHeaderView,
    shard_id: near_indexer_primitives::types::ShardId,
    balances_cache: crate::BalancesCache,
) -> anyhow::Result<Vec<BalanceChange>> {
    let mut result: Vec<BalanceChange> = vec![];

    for transaction in transactions {
        let outcome = &transaction.outcome.execution_outcome.outcome;
        let prev_balances: Balances = get_previous_balance(
            &outcome.executor_id,
            balances_cache.clone(),
            block_header.prev_hash,
        )
        .await?;

        let new_liquid_amount = prev_balances.0 - outcome.tokens_burnt;
        set_new_balances(
            outcome.executor_id.parse().unwrap(),
            (new_liquid_amount, prev_balances.1),
            balances_cache.clone(),
        ).await;

        result.push(BalanceChange {
            block_timestamp: block_header.timestamp.into(),
            receipt_id: None,
            transaction_hash: Some(transaction.transaction.hash.to_string()),
            affected_account_id: outcome.executor_id.to_string(),
            // todo is it ok?
            involved_account_id: None,
            direction: "ACTION_FROM_AFFECTED_ACCOUNT".to_string(),
            cause: "TRANSACTION_PROCESSING".to_string(),
            delta_liquid_amount: BigDecimal::from_str(
                &(-(outcome.tokens_burnt as i128)).to_string(),
            )
            .unwrap(),
            absolute_liquid_amount: BigDecimal::from_str(&new_liquid_amount.to_string()).unwrap(),
            delta_locked_amount: BigDecimal::zero(),
            absolute_locked_amount: BigDecimal::from_str(&prev_balances.1.to_string()).unwrap(),
            shard_id: shard_id as i32,
            // will enumerate later
            index_in_chunk: 0,
        });
    }

    // let action_receipt_actions: Vec<
    //     near_indexer_primitives::views::ReceiptView
    // > = receipts
    //     .iter()
    //     .filter_map(|receipt| {
    //         if let near_indexer_primitives::views::ReceiptEnumView::Action { actions, .. } =
    //         &receipt.receipt
    //         {
    //             Some(actions.iter().map(move |action| {
    //                 models::ActionReceiptAction::from_action_view(
    //                     receipt.receipt_id.to_string(),
    //                     action,
    //                     receipt.predecessor_id.to_string(),
    //                     receipt.receiver_id.to_string(),
    //                     block_hash,
    //                     block_timestamp,
    //                     chunk_header.shard_id as i32,
    //                     // we fill it later because we can't enumerate before filtering finishes
    //                     0,
    //                 )
    //             }))
    //         } else {
    //             None
    //         }
    //     })
    //     .flatten()
    //     .enumerate()
    //     .map(|(i, mut action)| {
    //         action.index_in_chunk = i as i32;
    //         action
    //     })
    //     .collect();

    Ok(result)
}

async fn get_previous_balance(
    account_id: &near_indexer_primitives::types::AccountId,
    balances_cache: crate::BalancesCache,
    prev_block_hash: near_indexer_primitives::CryptoHash,
) -> anyhow::Result<Balances> {
    // todo handle 11111111...
    let mut balances_cache_lock = balances_cache.lock().await;
    let prev_balances = match balances_cache_lock.cache_get(account_id) {
        None => {
            let account_view =
                get_account_view_for_block_hash(account_id, &prev_block_hash).await?;
            let balances = (account_view.amount, account_view.locked);
            balances_cache_lock.cache_set(account_id.clone(), balances);
            balances
        }
        Some(balances) => *balances,
    };
    drop(balances_cache_lock);
    Ok(prev_balances)
}

async fn set_new_balances(
    account_id: near_indexer_primitives::types::AccountId,
    balances: Balances,
    balances_cache: crate::BalancesCache,
) {
    let mut balances_cache_lock = balances_cache.lock().await;
    balances_cache_lock.cache_set(account_id, balances);
    drop(balances_cache_lock);
}

// todo add retry logic
async fn get_account_view_for_block_hash(
    account_id: &near_indexer_primitives::types::AccountId,
    block_hash: &near_indexer_primitives::CryptoHash,
) -> anyhow::Result<near_indexer_primitives::views::AccountView> {
    let block_reference = near_indexer_primitives::types::BlockReference::BlockId(
        near_indexer_primitives::types::BlockId::Hash(*block_hash),
    );
    let request = near_indexer_primitives::views::QueryRequest::ViewAccount {
        account_id: account_id.clone(),
    };
    let query = near_jsonrpc_client::methods::query::RpcQueryRequest {
        block_reference,
        request,
    };

    // todo
    let a = near_jsonrpc_client::JsonRpcClient::connect("https://archival-rpc.mainnet.near.org");

    let account_response = a.call(query).await?;
    match account_response.kind {
        near_jsonrpc_primitives::types::query::QueryResponseKind::ViewAccount(account) => {
            Ok(account)
        }
        _ => anyhow::bail!(
            "Failed to extract ViewAccount response for account {}, block {}",
            account_id,
            block_hash
        ),
    }
}
