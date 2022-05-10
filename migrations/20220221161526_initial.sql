-- https://docs.singlestore.com/db/v7.6/en/reference/sql-reference/data-definition-language-ddl/create-table.html--create-table
-- https://docs.singlestore.com/managed-service/en/reference/sql-reference/data-types/other-types.html

-- Short cheatsheet from the doc:
-- - all the tables in this project are columnstore tables
-- - Shard key is the way to identify which shard stores each row
-- - Unique key is a superset of the shard key
-- - There is only one sort key per table, and only one way to make fast range queries based on the sort
-- - Merging tables work fast if they have the same sort order -> we sort everything by timestamp
-- - `key ... using hash` gives fast queries by equality
-- - all the columns in hash keys should also be in shard key
-- - we don't use enum type because it' not allowed to use enums in keys
-- block_hash is shard_id everywhere because it make most of the joins faster
-- UNIQUE KEY usually contains block_hash because of SingleStore limitations

-- index_in_chunk, chunk_index_in_block may be partially broken because of multiple migrations, renaming, etc
-- it's ok for stress testing purposes, but for the production we need to run it from scratch

-- update_reason options:
--     {
--         'TRANSACTION_PROCESSING',
--         'ACTION_RECEIPT_PROCESSING_STARTED',
--         'ACTION_RECEIPT_GAS_REWARD',
--         'RECEIPT_PROCESSING',
--         'POSTPONED_RECEIPT',
--         'UPDATED_DELAYED_RECEIPTS',
--         'VALIDATOR_ACCOUNTS_UPDATE',
--         'MIGRATION',
--         'RESHARDING'
--     }
-- todo naming
CREATE TABLE account_changes
(
    account_id                 text           NOT NULL,
    block_timestamp            numeric(20, 0) NOT NULL,
    block_hash                 text           NOT NULL,
    caused_by_transaction_hash text,
    caused_by_receipt_id       text,
    update_reason              text           NOT NULL,
    nonstaked_balance          numeric(38, 0) NOT NULL,
    staked_balance             numeric(38, 0) NOT NULL,
    storage_usage              numeric(20, 0) NOT NULL,
    chunk_index_in_block       integer        NOT NULL,
    index_in_chunk             integer        NOT NULL
);

-- action_kind options:
--      {
--         'CREATE_ACCOUNT',
--         'DEPLOY_CONTRACT',
--         'FUNCTION_CALL',
--         'TRANSFER',
--         'STAKE',
--         'ADD_KEY',
--         'DELETE_KEY',
--         'DELETE_ACCOUNT'
--      }
CREATE TABLE action_receipts__actions
(
    block_hash             text           NOT NULL,
    block_timestamp        numeric(20, 0) NOT NULL,
    receipt_id             text           NOT NULL,
    action_kind            text           NOT NULL,
    -- https://docs.aws.amazon.com/redshift/latest/dg/json-functions.html
    -- https://docs.aws.amazon.com/redshift/latest/dg/super-overview.html
    args                   super           NOT NULL,
    predecessor_account_id text           NOT NULL,
    receiver_account_id    text           NOT NULL,
    chunk_index_in_block   integer        NOT NULL,
    index_in_chunk         integer        NOT NULL

-- TODO json field! + discuss indexes on json fields
-- https://docs.singlestore.com/db/v7.6/en/create-your-database/physical-database-schema-design/procedures-for-physical-database-schema-design/using-json.html#indexing-data-in-json-columns
-- CREATE INDEX action_receipt_actions_args_receiver_id_idx ON action_receipt_actions ((args -> 'args_json' ->> 'receiver_id')) WHERE action_receipt_actions.action_kind = 'FUNCTION_CALL' AND
--           (action_receipt_actions.args ->> 'args_json') IS NOT NULL;
);

CREATE TABLE action_receipts__outputs
(
    block_hash           text           NOT NULL,
    block_timestamp      numeric(20, 0) NOT NULL,
    receipt_id           text           NOT NULL,
    output_data_id       text           NOT NULL,
    receiver_account_id  text           NOT NULL,
    chunk_index_in_block integer        NOT NULL,
    index_in_chunk       integer        NOT NULL
);

CREATE TABLE action_receipts
(
    receipt_id                       text           NOT NULL,
    block_hash                       text           NOT NULL,
    chunk_hash                       text           NOT NULL,
    block_timestamp                  numeric(20, 0) NOT NULL,
    chunk_index_in_block             integer        NOT NULL,
    receipt_index_in_chunk           integer        NOT NULL, -- goes both through action and data receipts
    predecessor_account_id           text           NOT NULL,
    receiver_account_id              text           NOT NULL,
    originated_from_transaction_hash text           NOT NULL,
    signer_account_id                text           NOT NULL,
    signer_public_key                text           NOT NULL,
--     todo change logic with gas_price + gas_used
-- https://github.com/near/near-analytics/issues/19
    gas_price                        numeric(38, 0) NOT NULL
);

CREATE TABLE blocks
(
    block_height      numeric(20, 0) NOT NULL,
    block_hash        text           NOT NULL,
    prev_block_hash   text           NOT NULL,
    block_timestamp   numeric(20, 0) NOT NULL,
    total_supply      numeric(38, 0) NOT NULL,
--     todo next_block_gas_price? https://github.com/near/near-analytics/issues/19
    gas_price         numeric(38, 0) NOT NULL,
    author_account_id text           NOT NULL
);

CREATE TABLE chunks
(
    block_timestamp   numeric(20, 0) NOT NULL,
    block_hash        text           NOT NULL,
    chunk_hash        text           NOT NULL,
    index_in_block    integer        NOT NULL,
    signature         text           NOT NULL,
    gas_limit         numeric(20, 0) NOT NULL,
    gas_used          numeric(20, 0) NOT NULL,
    author_account_id text           NOT NULL
);

CREATE TABLE data_receipts
(
    receipt_id                       text           NOT NULL,
    block_hash                       text           NOT NULL,
    chunk_hash                       text           NOT NULL,
    block_timestamp                  numeric(20, 0) NOT NULL,
    chunk_index_in_block             integer        NOT NULL,
    receipt_index_in_chunk           integer        NOT NULL, -- goes both through action and data receipts
    predecessor_account_id           text           NOT NULL,
    receiver_account_id              text           NOT NULL,
    originated_from_transaction_hash text           NOT NULL,
    data_id                          text           NOT NULL,
    -- https://docs.singlestore.com/managed-service/en/reference/sql-reference/data-types/blob-types.html
    -- https://docs.aws.amazon.com/redshift/latest/dg/r_VARBYTE_type.html
    data                             varbyte
);

CREATE TABLE execution_outcomes__receipts
(
    block_hash           text           NOT NULL,
    block_timestamp      numeric(20, 0) NOT NULL,
    executed_receipt_id  text           NOT NULL,
    produced_receipt_id  text           NOT NULL,
    chunk_index_in_block integer        NOT NULL,
    index_in_chunk       integer        NOT NULL
);

-- status options:
--      {
--         'UNKNOWN',
--         'FAILURE',
--         'SUCCESS_VALUE',
--         'SUCCESS_RECEIPT_ID'
--      }
-- todo we want to store more data for this table and maybe for the others
CREATE TABLE execution_outcomes
(
    receipt_id           text           NOT NULL,
    block_hash           text           NOT NULL,
    block_timestamp      numeric(20, 0) NOT NULL,
    chunk_index_in_block integer        NOT NULL,
    index_in_chunk       integer        NOT NULL,
    gas_burnt            numeric(20, 0) NOT NULL,
    tokens_burnt         numeric(38, 0) NOT NULL,
    executor_account_id  text           NOT NULL,
    status               text           NOT NULL
);

-- status options:
--      {
--         'UNKNOWN',
--         'FAILURE',
--         'SUCCESS_VALUE',
--         'SUCCESS_RECEIPT_ID'
--      }
CREATE TABLE transactions
(
    transaction_hash                text           NOT NULL,
    block_hash                      text           NOT NULL,
    chunk_hash                      text           NOT NULL,
    block_timestamp                 numeric(20, 0) NOT NULL,
    chunk_index_in_block            integer        NOT NULL,
    index_in_chunk                  integer        NOT NULL,
    signer_account_id               text           NOT NULL,
    signer_public_key               text           NOT NULL,
    nonce                           numeric(20, 0) NOT NULL,
    receiver_account_id             text           NOT NULL,
    signature                       text           NOT NULL,
    status                          text           NOT NULL,
    converted_into_receipt_id       text           NOT NULL,
    receipt_conversion_gas_burnt    numeric(20, 0),
    receipt_conversion_tokens_burnt numeric(38, 0)
);