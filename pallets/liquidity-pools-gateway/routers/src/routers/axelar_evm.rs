// Copyright 2021 Centrifuge Foundation (centrifuge.io).
//
// This file is part of the Centrifuge chain project.
// Centrifuge is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version (see http://www.gnu.org/licenses).
// Centrifuge is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

use ethabi::{Contract, Function, Param, ParamType, Token};
use frame_support::{
	dispatch::{DispatchResult, DispatchResultWithPostInfo},
	pallet_prelude::DispatchError,
};
use frame_system::pallet_prelude::OriginFor;
use parity_scale_codec::{Decode, Encode, MaxEncodedLen};
use scale_info::{
	prelude::{format, string::String},
	TypeInfo,
};
use sp_core::{bounded::BoundedVec, ConstU32, H160};
use sp_std::{collections::btree_map::BTreeMap, vec, vec::Vec};

use crate::{
	EVMRouter, AXELAR_DESTINATION_CHAIN_PARAM, AXELAR_DESTINATION_CONTRACT_ADDRESS_PARAM,
	AXELAR_FUNCTION_NAME, AXELAR_PAYLOAD_PARAM, MAX_AXELAR_EVM_CHAIN_SIZE,
};

/// The router used for executing the LiquidityPools contract via Axelar.
#[derive(Debug, Encode, Decode, Clone, PartialEq, Eq, TypeInfo, MaxEncodedLen)]
pub struct AxelarEVMRouter<T>
where
	T: pallet_ethereum_transaction::Config + pallet_evm::Config,
	OriginFor<T>:
		From<pallet_ethereum::Origin> + Into<Result<pallet_ethereum::Origin, OriginFor<T>>>,
{
	pub router: EVMRouter<T>,
	pub evm_chain: BoundedVec<u8, ConstU32<MAX_AXELAR_EVM_CHAIN_SIZE>>,
	pub liquidity_pools_contract_address: H160,
}

impl<T> AxelarEVMRouter<T>
where
	T: pallet_ethereum_transaction::Config + pallet_evm::Config,
	T::AccountId: AsRef<[u8; 32]>,
	OriginFor<T>:
		From<pallet_ethereum::Origin> + Into<Result<pallet_ethereum::Origin, OriginFor<T>>>,
{
	pub fn new(
		router: EVMRouter<T>,
		evm_chain: BoundedVec<u8, ConstU32<MAX_AXELAR_EVM_CHAIN_SIZE>>,
		liquidity_pools_contract_address: H160,
	) -> Self {
		Self {
			router,
			evm_chain,
			liquidity_pools_contract_address,
		}
	}

	/// Calls the init function on the EVM router.
	pub fn do_init(&self) -> DispatchResult {
		self.router.do_init()
	}

	/// Executes the EVM call using the generic EVM router.
	pub fn do_send(&self, sender: T::AccountId, msg: Vec<u8>) -> DispatchResultWithPostInfo {
		let eth_msg = get_axelar_encoded_msg(
			msg,
			self.evm_chain.clone().into_inner(),
			self.liquidity_pools_contract_address,
		)
		.map_err(DispatchError::Other)?;

		self.router.do_send(sender, eth_msg)
	}
}

/// Encodes the provided message into the format required for submitting it
/// to the Axelar contract which in turn calls the LiquidityPools
/// contract with the serialized LP message as `payload`.
///
/// Axelar contract call:
/// <https://github.com/axelarnetwork/axelar-cgp-solidity/blob/v4.3.2/contracts/AxelarGateway.sol#L78>
///
/// LiquidityPools contract call:
/// <https://github.com/centrifuge/liquidity-pools/blob/383d279f809a01ab979faf45f31bf9dc3ce6a74a/src/routers/Gateway.sol#L276>
pub(crate) fn get_axelar_encoded_msg(
	serialized_msg: Vec<u8>,
	target_chain: Vec<u8>,
	target_contract: H160,
) -> Result<Vec<u8>, &'static str> {
	#[allow(deprecated)]
	let encoded_axelar_contract = Contract {
		constructor: None,
		functions: BTreeMap::<String, Vec<Function>>::from([(
			AXELAR_FUNCTION_NAME.into(),
			vec![Function {
				name: AXELAR_FUNCTION_NAME.into(),
				inputs: vec![
					Param {
						name: AXELAR_DESTINATION_CHAIN_PARAM.into(),
						kind: ParamType::String,
						internal_type: None,
					},
					Param {
						name: AXELAR_DESTINATION_CONTRACT_ADDRESS_PARAM.into(),
						kind: ParamType::String,
						internal_type: None,
					},
					Param {
						name: AXELAR_PAYLOAD_PARAM.into(),
						kind: ParamType::Bytes,
						internal_type: None,
					},
				],
				outputs: vec![],
				constant: Some(false),
				state_mutability: Default::default(),
			}],
		)]),
		events: Default::default(),
		errors: Default::default(),
		receive: false,
		fallback: false,
	}
	.function(AXELAR_FUNCTION_NAME)
	.map_err(|_| "cannot retrieve Axelar contract function")?
	.encode_input(&[
		Token::String(
			String::from_utf8(target_chain).map_err(|_| "target chain conversion error")?,
		),
		// Ensure that the target contract is correctly converted to hex.
		//
		// The `to_string` method on the H160 is returning a string containing an ellipsis, such
		// as: 0x1234…7890
		Token::String(format!("0x{}", hex::encode(target_contract.0))),
		Token::Bytes(serialized_msg),
	])
	.map_err(|_| "cannot encode input for Axelar contract function")?;

	Ok(encoded_axelar_contract)
}
