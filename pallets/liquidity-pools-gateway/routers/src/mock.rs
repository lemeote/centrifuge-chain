use std::str::FromStr;

use cfg_mocks::{pallet_mock_liquidity_pools, pallet_mock_routers};
use cfg_primitives::{BLOCK_STORAGE_LIMIT, MAX_POV_SIZE};
use cfg_types::domain_address::DomainAddress;
use cumulus_primitives_core::{Instruction, PalletInstance, Parachain, SendError, Xcm, XcmHash};
use frame_support::{
	derive_impl, parameter_types,
	traits::{FindAuthor, PalletInfo as PalletInfoTrait},
	weights::Weight,
};
use frame_system::EnsureRoot;
use pallet_ethereum::{IntermediateStateRoot, PostLogContent};
use pallet_evm::{
	runner::stack::Runner, AddressMapping, EnsureAddressNever, EnsureAddressRoot, FeeCalculator,
	FixedGasWeightMapping, IsPrecompileResult, Precompile, PrecompileHandle, PrecompileResult,
	PrecompileSet, SubstrateBlockHashMapping,
};
use parity_scale_codec::{Decode, Encode};
use sp_core::{crypto::AccountId32, ByteArray, ConstU32, H160, U256};
use sp_runtime::{traits::IdentityLookup, ConsensusEngineId};
use sp_std::{cell::RefCell, marker::PhantomData};
use staging_xcm::latest::{
	opaque, Asset, Assets, Error as XcmError, InteriorLocation, Junction, Location, NetworkId,
	Result as XcmResult, SendResult, SendXcm, XcmContext,
};
use staging_xcm_executor::{
	traits::{TransactAsset, WeightBounds},
	AssetsInHolding,
};
use xcm_primitives::{
	HrmpAvailableCalls, HrmpEncodeCall, UtilityAvailableCalls, UtilityEncodeCall, XcmTransact,
};

pub type Balance = u128;

frame_support::construct_runtime!(
	pub enum Runtime {
		System: frame_system,
		Balances: pallet_balances,
		MockLiquidityPools: pallet_mock_liquidity_pools,
		XcmTransactor: pallet_xcm_transactor,
		EVM: pallet_evm,
		Timestamp: pallet_timestamp,
		Ethereum: pallet_ethereum,
		EthereumTransaction: pallet_ethereum_transaction,
	}
);

frame_support::parameter_types! {
	pub const MaxIncomingMessageSize: u32 = 1024;
}

#[derive_impl(frame_system::config_preludes::TestDefaultConfig as frame_system::DefaultConfig)]
impl frame_system::Config for Runtime {
	type AccountData = pallet_balances::AccountData<Balance>;
	type AccountId = AccountId32;
	type Block = frame_system::mocking::MockBlock<Runtime>;
	type Lookup = IdentityLookup<Self::AccountId>;
}

parameter_types! {
	// the minimum fee for an anchor is 500,000ths of a CFG.
	// This is set to a value so you can still get some return without getting your account removed.
	pub const ExistentialDeposit: Balance = 1 * cfg_primitives::MICRO_CFG;
}

#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig as pallet_balances::DefaultConfig)]
impl pallet_balances::Config for Runtime {
	type AccountStore = System;
	type Balance = Balance;
	type DustRemoval = ();
	type ExistentialDeposit = ExistentialDeposit;
	type RuntimeHoldReason = ();
}

impl pallet_mock_liquidity_pools::Config for Runtime {
	type DomainAddress = DomainAddress;
	type Message = ();
}

impl pallet_ethereum_transaction::Config for Runtime {}

impl pallet_mock_routers::Config for Runtime {}

parameter_types! {
	pub const MinimumPeriod: u64 = 1000;
}

impl pallet_timestamp::Config for Runtime {
	type MinimumPeriod = MinimumPeriod;
	type Moment = u64;
	type OnTimestampSet = ();
	type WeightInfo = ();
}

///////////////////////
// EVM pallet mocks. //
///////////////////////

pub struct FixedGasPrice;
impl FeeCalculator for FixedGasPrice {
	fn min_gas_price() -> (U256, Weight) {
		// Return some meaningful gas price and weight
		(1_000_000_000u128.into(), Weight::from_parts(7u64, 0))
	}
}

/// Identity address mapping.
pub struct IdentityAddressMapping;

impl AddressMapping<AccountId32> for IdentityAddressMapping {
	fn into_account_id(address: H160) -> AccountId32 {
		let tag = b"EVM";
		let mut bytes = [0; 32];
		bytes[0..20].copy_from_slice(address.as_bytes());
		bytes[20..28].copy_from_slice(&1u64.to_be_bytes());
		bytes[28..31].copy_from_slice(tag);

		AccountId32::from_slice(bytes.as_slice()).unwrap()
	}
}

pub struct FindAuthorTruncated;
impl FindAuthor<H160> for FindAuthorTruncated {
	fn find_author<'a, I>(_digests: I) -> Option<H160>
	where
		I: 'a + IntoIterator<Item = (ConsensusEngineId, &'a [u8])>,
	{
		Some(H160::from_str("1234500000000000000000000000000000000000").unwrap())
	}
}

pub struct MockPrecompileSet;

impl PrecompileSet for MockPrecompileSet {
	/// Tries to execute a precompile in the precompile set.
	/// If the provided address is not a precompile, returns None.
	fn execute(&self, handle: &mut impl PrecompileHandle) -> Option<PrecompileResult> {
		let address = handle.code_address();

		if address == H160::from_low_u64_be(1) {
			return Some(pallet_evm_precompile_simple::Identity::execute(handle));
		}

		None
	}

	/// Check if the given address is a precompile. Should only be called to
	/// perform the check while not executing the precompile afterward, since
	/// `execute` already performs a check internally.
	fn is_precompile(&self, address: H160, _remaining_gas: u64) -> IsPrecompileResult {
		IsPrecompileResult::Answer {
			is_precompile: address == H160::from_low_u64_be(1),
			extra_cost: 0,
		}
	}
}

parameter_types! {
	pub BlockGasLimit: U256 = U256::max_value();
	pub WeightPerGas: Weight = Weight::from_parts(20_000, 0);
	pub MockPrecompiles: MockPrecompileSet = MockPrecompileSet;
	pub GasLimitPovSizeRatio: u64 = {
		let block_gas_limit = BlockGasLimit::get().min(u64::MAX.into()).low_u64();
		block_gas_limit.saturating_div(MAX_POV_SIZE)
	};
	pub GasLimitStorageGrowthRatio: u64 =
		BlockGasLimit::get().min(u64::MAX.into()).low_u64().saturating_div(BLOCK_STORAGE_LIMIT);
}

impl pallet_evm::Config for Runtime {
	type AddressMapping = IdentityAddressMapping;
	type BlockGasLimit = BlockGasLimit;
	type BlockHashMapping = SubstrateBlockHashMapping<Self>;
	type CallOrigin = EnsureAddressRoot<Self::AccountId>;
	type ChainId = ();
	type Currency = Balances;
	type FeeCalculator = FixedGasPrice;
	type FindAuthor = FindAuthorTruncated;
	type GasLimitPovSizeRatio = GasLimitPovSizeRatio;
	type GasLimitStorageGrowthRatio = GasLimitStorageGrowthRatio;
	type GasWeightMapping = FixedGasWeightMapping<Self>;
	type OnChargeTransaction = ();
	type OnCreate = ();
	type PrecompilesType = MockPrecompileSet;
	type PrecompilesValue = MockPrecompiles;
	type Runner = Runner<Self>;
	type RuntimeEvent = RuntimeEvent;
	type SuicideQuickClearLimit = ConstU32<0>;
	type Timestamp = Timestamp;
	type WeightInfo = ();
	type WeightPerGas = WeightPerGas;
	type WithdrawOrigin = EnsureAddressNever<Self::AccountId>;
}

parameter_types! {
	pub const PostBlockAndTxnHashes: PostLogContent = PostLogContent::BlockAndTxnHashes;
	pub const ExtraDataLength: u32 = 30;
}

impl pallet_ethereum::Config for Runtime {
	type ExtraDataLength = ExtraDataLength;
	type PostLogContent = PostBlockAndTxnHashes;
	type RuntimeEvent = RuntimeEvent;
	type StateRoot = IntermediateStateRoot<Self>;
}
///////////////////////////
// XCM transactor mocks. //
///////////////////////////

// Transactors for the mock runtime. Only relay chain
#[derive(Clone, Eq, Debug, PartialEq, Ord, PartialOrd, Encode, Decode, scale_info::TypeInfo)]
pub enum Transactors {
	Relay,
}

#[cfg(feature = "runtime-benchmarks")]
impl Default for Transactors {
	fn default() -> Self {
		Transactors::Relay
	}
}

impl XcmTransact for Transactors {
	fn destination(self) -> Location {
		match self {
			Transactors::Relay => Location::parent(),
		}
	}
}

impl UtilityEncodeCall for Transactors {
	fn encode_call(self, call: UtilityAvailableCalls) -> Vec<u8> {
		match self {
			Transactors::Relay => match call {
				UtilityAvailableCalls::AsDerivative(a, b) => {
					let mut call =
						RelayCall::Utility(UtilityCall::AsDerivative(a.clone())).encode();
					call.append(&mut b.clone());
					call
				}
			},
		}
	}
}

pub struct AccountIdToLocation;
impl sp_runtime::traits::Convert<AccountId32, Location> for AccountIdToLocation {
	fn convert(_account: AccountId32) -> Location {
		let as_h160: H160 = H160::repeat_byte(0xAA);
		Location::new(
			0,
			Junction::AccountKey20 {
				network: None,
				key: as_h160.as_fixed_bytes().clone(),
			},
		)
	}
}

pub struct DummyAssetTransactor;
impl TransactAsset for DummyAssetTransactor {
	fn deposit_asset(_what: &Asset, _who: &Location, _context: Option<&XcmContext>) -> XcmResult {
		Ok(())
	}

	fn withdraw_asset(
		_what: &Asset,
		_who: &Location,
		_context: Option<&XcmContext>,
	) -> Result<AssetsInHolding, XcmError> {
		Ok(AssetsInHolding::default())
	}
}

pub struct CurrencyIdToLocation;

pub type AssetId = u128;

#[derive(Clone, Eq, Debug, PartialEq, Ord, PartialOrd, Encode, Decode, scale_info::TypeInfo)]
pub enum CurrencyId {
	SelfReserve,
	OtherReserve(AssetId),
}

impl sp_runtime::traits::Convert<CurrencyId, Option<Location>> for CurrencyIdToLocation {
	fn convert(currency: CurrencyId) -> Option<Location> {
		match currency {
			CurrencyId::SelfReserve => {
				let multi: Location = SelfReserve::get();
				Some(multi)
			}
			// To distinguish between relay and others, specially for reserve asset
			CurrencyId::OtherReserve(asset) => {
				if asset == 0 {
					Some(Location::parent())
				} else {
					Some(Location::new(1, Parachain(2)))
				}
			}
		}
	}
}

pub struct MockHrmpEncoder;

impl HrmpEncodeCall for MockHrmpEncoder {
	fn hrmp_encode_call(call: HrmpAvailableCalls) -> Result<Vec<u8>, XcmError> {
		match call {
			HrmpAvailableCalls::InitOpenChannel(_, _, _) => {
				Ok(RelayCall::Hrmp(HrmpCall::Init()).encode())
			}
			HrmpAvailableCalls::AcceptOpenChannel(_) => {
				Ok(RelayCall::Hrmp(HrmpCall::Accept()).encode())
			}
			HrmpAvailableCalls::CloseChannel(_) => Ok(RelayCall::Hrmp(HrmpCall::Close()).encode()),
			HrmpAvailableCalls::CancelOpenRequest(_, _) => {
				Ok(RelayCall::Hrmp(HrmpCall::Close()).encode())
			}
		}
	}
}

// Simulates sending a XCM message
thread_local! {
	pub static SENT_XCM: RefCell<Vec<(Location, opaque::Xcm)>> = RefCell::new(Vec::new());
}
pub fn sent_xcm() -> Vec<(Location, opaque::Xcm)> {
	SENT_XCM.with(|q| (*q.borrow()).clone())
}
pub struct TestSendXcm;
impl SendXcm for TestSendXcm {
	type Ticket = ();

	fn validate(
		destination: &mut Option<Location>,
		message: &mut Option<opaque::Xcm>,
	) -> SendResult<Self::Ticket> {
		SENT_XCM.with(|q| {
			q.borrow_mut()
				.push((destination.clone().unwrap(), message.clone().unwrap()))
		});
		Ok(((), Assets::new()))
	}

	fn deliver(_: Self::Ticket) -> Result<XcmHash, SendError> {
		Ok(XcmHash::default())
	}
}

#[derive(Encode, Decode)]
pub enum RelayCall {
	#[codec(index = 0u8)]
	// the index should match the position of the module in `construct_runtime!`
	Utility(UtilityCall),
	#[codec(index = 1u8)]
	// the index should match the position of the module in `construct_runtime!`
	Hrmp(HrmpCall),
}

#[derive(Encode, Decode)]
pub enum UtilityCall {
	#[codec(index = 0u8)]
	AsDerivative(u16),
}

#[derive(Encode, Decode)]
pub enum HrmpCall {
	#[codec(index = 0u8)]
	Init(),
	#[codec(index = 1u8)]
	Accept(),
	#[codec(index = 2u8)]
	Close(),
}

pub type MaxHrmpRelayFee = staging_xcm_builder::Case<MaxFee>;

pub struct DummyWeigher<C>(PhantomData<C>);

impl<C: Decode> WeightBounds<C> for DummyWeigher<C> {
	fn weight(_message: &mut Xcm<C>) -> Result<staging_xcm::latest::Weight, ()> {
		Ok(Weight::zero())
	}

	fn instr_weight(_instruction: &Instruction<C>) -> Result<staging_xcm::latest::Weight, ()> {
		Ok(Weight::zero())
	}
}

parameter_types! {
	pub const RelayNetwork: NetworkId = NetworkId::Polkadot;

	pub ParachainId: cumulus_primitives_core::ParaId = 100.into();

	pub SelfLocation: Location =
		Location::new(1, Parachain(ParachainId::get().into()));

	pub SelfReserve: Location = Location::new(
		1,
		[
			Parachain(ParachainId::get().into()),
			PalletInstance(
				<Runtime as frame_system::Config>::PalletInfo::index::<Balances>().unwrap() as u8
			)
		]
	);

	pub const BaseXcmWeight: staging_xcm::latest::Weight = staging_xcm::latest::Weight::from_parts(1000, 0);

	pub MaxFee: Asset = (Location::parent(), 1_000_000_000_000u128).into();

	pub UniversalLocation: InteriorLocation = RelayNetwork::get().into();
}

impl pallet_xcm_transactor::Config for Runtime {
	type AccountIdToLocation = AccountIdToLocation;
	type AssetTransactor = DummyAssetTransactor;
	type Balance = Balance;
	type BaseXcmWeight = BaseXcmWeight;
	type CurrencyId = CurrencyId;
	type CurrencyIdToLocation = CurrencyIdToLocation;
	type DerivativeAddressRegistrationOrigin = EnsureRoot<AccountId32>;
	type HrmpManipulatorOrigin = EnsureRoot<AccountId32>;
	type HrmpOpenOrigin = EnsureRoot<AccountId32>;
	type MaxHrmpFee = MaxHrmpRelayFee;
	type ReserveProvider = orml_traits::location::RelativeReserveProvider;
	type RuntimeEvent = RuntimeEvent;
	type SelfLocation = SelfLocation;
	type SovereignAccountDispatcherOrigin = EnsureRoot<AccountId32>;
	type Transactor = Transactors;
	type UniversalLocation = UniversalLocation;
	type Weigher = DummyWeigher<RuntimeCall>;
	type WeightInfo = ();
	type XcmSender = TestSendXcm;
}

pub fn new_test_ext() -> sp_io::TestExternalities {
	System::externalities()
}
