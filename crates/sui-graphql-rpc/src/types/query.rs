// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use async_graphql::{connection::Connection, *};

use super::{
    address::Address,
    checkpoint::{Checkpoint, CheckpointId},
    epoch::Epoch,
    object::Object,
    owner::ObjectOwner,
    protocol_config::ProtocolConfigs,
    sui_address::SuiAddress,
    transaction_block::TransactionBlock,
};
use crate::{
    config::ServiceConfig,
    context_data::{context_ext::DataProviderContextExt, db_data_provider::PgManager},
    error::{code, graphql_error, Error},
};

pub(crate) struct Query;
pub(crate) type SuiGraphQLSchema = async_graphql::Schema<Query, EmptyMutation, EmptySubscription>;

#[allow(unreachable_code)]
#[allow(unused_variables)]
#[Object]
impl Query {
    /// First four bytes of the network's genesis checkpoint digest (uniquely identifies the
    /// network).
    async fn chain_identifier(&self, ctx: &Context<'_>) -> Result<String> {
        ctx.data_provider().fetch_chain_id().await
    }

    /// Configuration for this RPC service
    async fn service_config(&self, ctx: &Context<'_>) -> Result<ServiceConfig> {
        Ok(ctx
            .data()
            .map_err(|_| {
                graphql_error(
                    code::INTERNAL_SERVER_ERROR,
                    "Unable to fetch service configuration",
                )
            })
            .cloned()?)
    }

    async fn owner(&self, ctx: &Context<'_>, address: SuiAddress) -> Result<Option<ObjectOwner>> {
        // Currently only an account address can own an object
        let o = ctx.data_provider().fetch_obj(address, None).await?;
        Ok(o.and_then(|q| q.owner)
            .map(|o| ObjectOwner::Address(Address { address: o })))
    }

    async fn object(
        &self,
        ctx: &Context<'_>,
        address: SuiAddress,
        version: Option<u64>,
    ) -> Result<Option<Object>> {
        ctx.data_provider().fetch_obj(address, version).await
    }

    async fn address(&self, address: SuiAddress) -> Option<Address> {
        Some(Address { address })
    }

    async fn transaction_block(
        &self,
        ctx: &Context<'_>,
        digest: String,
    ) -> Result<Option<TransactionBlock>> {
        let result = ctx.data_unchecked::<PgManager>().fetch_tx(&digest).await?;
        result.map(TransactionBlock::try_from).transpose().extend()
    }

    async fn epoch(&self, ctx: &Context<'_>, id: Option<u64>) -> Result<Option<Epoch>> {
        let result = if let Some(epoch_id) = id {
            ctx.data_unchecked::<PgManager>()
                .fetch_epoch(epoch_id)
                .await?
        } else {
            Some(
                ctx.data_unchecked::<PgManager>()
                    .fetch_latest_epoch()
                    .await?,
            )
        };
        Ok(result.map(Epoch::from))
    }

    async fn checkpoint(
        &self,
        ctx: &Context<'_>,
        id: Option<CheckpointId>,
    ) -> Result<Option<Checkpoint>> {
        let result = if let Some(id) = id {
            match (&id.digest, &id.sequence_number) {
                (Some(_), Some(_)) => return Err(Error::InvalidCheckpointQuery.extend()),
                _ => {
                    ctx.data_unchecked::<PgManager>()
                        .fetch_checkpoint(id.digest.as_deref(), id.sequence_number)
                        .await?
                }
            }
        } else {
            Some(
                ctx.data_unchecked::<PgManager>()
                    .fetch_latest_checkpoint()
                    .await?,
            )
        };

        result.map(Checkpoint::try_from).transpose().extend()
    }

    async fn checkpoint_connection(
        &self,
        ctx: &Context<'_>,
        first: Option<u64>,
        after: Option<String>,
        last: Option<u64>,
        before: Option<String>,
    ) -> Result<Connection<String, Checkpoint>> {
        ctx.data_provider()
            .fetch_checkpoint_connection(first, after, last, before)
            .await
    }

    async fn protocol_config(
        &self,
        ctx: &Context<'_>,
        protocol_version: Option<u64>,
    ) -> Result<ProtocolConfigs> {
        ctx.data_provider()
            .fetch_protocol_config(protocol_version)
            .await
    }
}
