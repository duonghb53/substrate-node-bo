#![cfg_attr(not(feature = "std"), no_std)]

/// Edit this file to define custom logic or remove it if it is not needed.
/// Learn more about FRAME and the core library of Substrate FRAME pallets:
/// <https://docs.substrate.io/v3/runtime/frame>
pub use pallet::*;

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

#[cfg(feature = "runtime-benchmarks")]
mod benchmarking;








/// Based on the above `KeyTypeId` we need to generate a pallet-specific crypto type wrappers.
/// We can use from supported crypto kinds (`sr25519`, `ed25519` and `ecdsa`) and augment
/// the types with this pallet-specific identifier.
pub mod crypto {
	use sp_core::crypto::KeyTypeId;
	use frame_system::offchain::AppCrypto;
	use sp_core::sr25519::Signature as Sr25519Signature;
	use sp_runtime::{
		app_crypto::{app_crypto, sr25519},
		traits::Verify,
		MultiSignature, MultiSigner,
	};

	/// Defines application identifier for crypto keys of this module.
	///
	/// Every module that deals with signatures needs to declare its unique identifier for
	/// its crypto keys.
	/// When offchain worker is signing transactions it's going to request keys of type
	/// `KeyTypeId` from the keystore and use the ones it finds to sign the transaction.
	/// The keys can be inserted manually via RPC (see `author_insertKey`).
	pub const KEY_TYPE: KeyTypeId = KeyTypeId(*b"btc!");


	app_crypto!(sr25519, KEY_TYPE);

	pub struct TestAuthId;

	impl AppCrypto<MultiSigner, MultiSignature> for TestAuthId {
		type RuntimeAppPublic = Public;
		type GenericSignature = sp_core::sr25519::Signature;
		type GenericPublic = sp_core::sr25519::Public;
	}

	// implemented for mock runtime in test
	impl AppCrypto<<Sr25519Signature as Verify>::Signer, Sr25519Signature> for TestAuthId {
		type RuntimeAppPublic = Public;
		type GenericSignature = sp_core::sr25519::Signature;
		type GenericPublic = sp_core::sr25519::Public;
	}
}









#[frame_support::pallet]
pub mod pallet {
	use scale_info::prelude::collections::VecDeque;
	use frame_support::pallet_prelude::*;
	use frame_system::pallet_prelude::*;
	use sp_std::vec::Vec;
	use scale_info::prelude::string::String; // support String
	use frame_system::{
		self as system,
		offchain::{
			AppCrypto, CreateSignedTransaction, SendSignedTransaction, SendUnsignedTransaction,
			SignedPayload, Signer, SigningTypes, SubmitTransaction,
		},
	};
	use lite_json::JsonValue;
	use sp_runtime::{
		offchain::{
			http,
			storage::{MutateStorageError, StorageRetrievalError, StorageValueRef},
			Duration,
		},
		traits::Zero,
		transaction_validity::{InvalidTransaction, TransactionValidity, ValidTransaction},
		RuntimeDebug,
		SaturatedConversion,
	};

	/// Configure the pallet by specifying the parameters and types on which it depends.
	#[pallet::config]
	// pub trait Config: frame_system::Config {
	pub trait Config: frame_system::Config + CreateSignedTransaction<Call<Self>> {
		/// Because this pallet emits events, it depends on the runtime's definition of an event.
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;

		/// The identifier type for an offchain worker.
		type AuthorityId: AppCrypto<Self::Public, Self::Signature>;

		/// The overarching dispatch call type.
		type Call: From<Call<Self>>;

		// Configuration parameters

		/// A grace period after we send transaction.
		///
		/// To avoid sending too many transactions, we only attempt to send one
		/// every `GRACE_PERIOD` blocks. We use Local Storage to coordinate
		/// sending between distinct runs of this offchain worker.
		#[pallet::constant]
		type GracePeriod: Get<Self::BlockNumber>;

		/// Number of blocks of cooldown after unsigned transaction is included.
		///
		/// This ensures that we only accept unsigned transactions once, every `UnsignedInterval`
		/// blocks.
		#[pallet::constant]
		type UnsignedInterval: Get<Self::BlockNumber>;

		/// A configuration for base priority of unsigned transactions.
		///
		/// This is exposed so that it can be tuned for particular runtime, when
		/// multiple pallets send unsigned transactions.
		#[pallet::constant]
		type UnsignedPriority: Get<TransactionPriority>;

		/// Maximum number of prices.
		#[pallet::constant]
		type MaxPrices: Get<u32>;
	}

	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
	pub struct Pallet<T>(_);



	// Payload used by this example crate to hold price
	/// data required to submit a transaction.
	#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, scale_info::TypeInfo)]
	pub struct PricePayload<Public, BlockNumber> {
		block_number: BlockNumber,
		price: u32,
		public: Public,
	}

	// impl<T: SigningTypes> Encode for PricePayload<frame_system::offchain::Public, frame_system::pallet::BlockNumber> {}

	// Ignore the error: the trait bound `PricePayload<…, …>: Encode` is not satisfied [E0277] the trait `Encode` is not implemented for `PricePayload<…, …>`
	impl<T: SigningTypes> SignedPayload<T> for PricePayload<T::Public, T::BlockNumber> {
		fn public(&self) -> T::Public {
			self.public.clone()
		}
	}

	enum TransactionType {
		Signed,
		UnsignedForAny,
		UnsignedForAll,
		Raw,
		None,
	}



	// The pallet's runtime storage items.
	// https://docs.substrate.io/v3/runtime/storage
	#[pallet::storage]
	#[pallet::getter(fn something)]
	// Learn more about declaring storage items:
	// https://docs.substrate.io/v3/runtime/storage#declaring-storage-items
	pub type Something<T> = StorageValue<_, u32>;

	/// A vector of recently submitted prices.
	///
	/// This is used to calculate average price, should have bounded size.
	#[pallet::storage]
	#[pallet::getter(fn prices)]
	// pub(super) type Prices<T: Config> = StorageValue<_, BoundedVec<u32, T::MaxPrices>, ValueQuery>;
	pub(super) type Prices<T: Config> = StorageValue<_, VecDeque<u32>, ValueQuery>;

	/// Predict the next price using EMA
	/// Why?
	/// 	We need a realtime approximately price value => this is the best method
	#[pallet::storage]
	#[pallet::getter(fn next_predicted_price)]
	pub(super) type NextPredictedPrice<T: Config> = StorageValue<_, (u32, T::BlockNumber), ValueQuery>;

	/// Defines the block when next unsigned transaction will be accepted.
	///
	/// To prevent spam of unsigned (and unpayed!) transactions on the network,
	/// we only allow one transaction every `T::UnsignedInterval` blocks.
	/// This storage entry defines when new transaction is going to be accepted.
	#[pallet::storage]
	#[pallet::getter(fn next_unsigned_at)]
	pub(super) type NextUnsignedAt<T: Config> = StorageValue<_, T::BlockNumber, ValueQuery>;



	// Pallets use events to inform users when important changes are made.
	// https://docs.substrate.io/v3/runtime/events-and-errors
	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		/// Event documentation should end with an array that provides descriptive names for event
		/// parameters. [something, who]
		SomethingStored(u32, T::AccountId),

		/// Event generated when new price is accepted to contribute to the average.
		NewPrice { price: u32, maybe_who: Option<T::AccountId> },
	}

	// Errors inform users that something went wrong.
	#[pallet::error]
	pub enum Error<T> {
		/// Error names should be descriptive.
		NoneValue,
		/// Errors should have helpful documentation associated with them.
		StorageOverflow,

		/// Symbol was not supported
		NotSupportedSymbol,
	}


	#[pallet::validate_unsigned]
	impl<T: Config> ValidateUnsigned for Pallet<T> {
		type Call = Call<T>;

		/// Validate unsigned call to this module.
		///
		/// By default unsigned transactions are disallowed, but implementing the validator
		/// here we make sure that some particular calls (the ones produced by offchain worker)
		/// are being whitelisted and marked as valid.
		fn validate_unsigned(_source: TransactionSource, call: &Self::Call) -> TransactionValidity {
			// Firstly let's check that we call the right function.
			if let Call::submit_price_unsigned_with_signed_payload {
				price_payload: ref payload,
				ref signature,
			} = call
			{
				let signature_valid =
					SignedPayload::<T>::verify::<T::AuthorityId>(payload, signature.clone());
				if !signature_valid {
					return InvalidTransaction::BadProof.into()
				}
				Self::validate_transaction_parameters(&payload.block_number, &payload.price)
			} else if let Call::submit_price_unsigned { block_number, price: new_price } = call {
				Self::validate_transaction_parameters(block_number, new_price)
			} else {
				InvalidTransaction::Call.into()
			}
		}
	}


	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		/// Offchain Worker entry point.
		///
		/// By implementing `fn offchain_worker` you declare a new offchain worker.
		/// This function will be called when the node is fully synced and a new best block is
		/// succesfuly imported.
		/// Note that it's not guaranteed for offchain workers to run on EVERY block, there might
		/// be cases where some blocks are skipped, or for some the worker runs twice (re-orgs),
		/// so the code should be able to handle that.
		/// You can use `Local Storage` API to coordinate runs of the worker.
		fn offchain_worker(block_number: T::BlockNumber) {
			// Note that having logs compiled to WASM may cause the size of the blob to increase
			// significantly. You can use `RuntimeDebug` custom derive to hide details of the types
			// in WASM. The `sp-api` crate also provides a feature `disable-logging` to disable
			// all logging and thus, remove any logging from the WASM.
			log::info!("Offchain workers hook: block_number: {:?}", block_number);

			// Since off-chain workers are just part of the runtime code, they have direct access
			// to the storage and other included pallets.
			//
			// We can easily import `frame_system` and retrieve a block hash of the parent block.
			let parent_hash = <system::Pallet<T>>::block_hash(block_number - 1u32.into());
			log::debug!("Current block: {:?} (parent hash: {:?})", block_number, parent_hash);

			// It's a good practice to keep `fn offchain_worker()` function minimal, and move most
			// of the code to separate `impl` block.
			// Here we call a helper function to calculate current average price.
			// This function reads storage entries of the current state.
			// let average: Option<u32> = Self::average_price();
			// log::debug!("Current price: {:?}", average);

			// For this example we are going to send both signed and unsigned transactions
			// depending on the block number.
			// Usually it's enough to choose one or the other.
			let should_send = Self::choose_transaction_type(block_number);
			let res = match should_send {
				// TransactionType::Signed => Self::fetch_price_and_send_signed(),
				// TransactionType::UnsignedForAny =>
				// 	Self::fetch_price_and_send_unsigned_for_any_account(block_number),
				// TransactionType::UnsignedForAll =>
				// 	Self::fetch_price_and_send_unsigned_for_all_accounts(block_number),
				TransactionType::Raw => Self::fetch_price_and_send_raw_unsigned(block_number),
				TransactionType::None => Ok(()),
				_ => Err("Invalid tx type"),
			};
			if let Err(e) = res {
				log::error!("Error: {}", e);
			}
		}
	}

	// Dispatchable functions allows users to interact with the pallet and invoke state changes.
	// These functions materialize as "extrinsics", which are often compared to transactions.
	// Dispatchable functions must be annotated with a weight and must return a DispatchResult.
	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// An example dispatchable that takes a singles value as a parameter, writes the value to
		/// storage and emits an event. This function must be dispatched by a signed extrinsic.
		#[pallet::weight(10_000 + T::DbWeight::get().writes(1))]
		pub fn do_something(origin: OriginFor<T>, something: u32) -> DispatchResult {
			// Check that the extrinsic was signed and get the signer.
			// This function will return an error if the extrinsic is not signed.
			// https://docs.substrate.io/v3/runtime/origins
			let who = ensure_signed(origin)?;

			// Update storage.
			<Something<T>>::put(something);

			// Emit an event.
			Self::deposit_event(Event::SomethingStored(something, who));
			// Return a successful DispatchResultWithPostInfo
			Ok(())
		}

		/// An example dispatchable that may throw a custom error.
		#[pallet::weight(10_000 + T::DbWeight::get().reads_writes(1,1))]
		pub fn cause_error(origin: OriginFor<T>) -> DispatchResult {
			let _who = ensure_signed(origin)?;

			// Read a value from storage.
			match <Something<T>>::get() {
				// Return an error if the value has not been set.
				None => Err(Error::<T>::NoneValue)?,
				Some(old) => {
					// Increment the value read from storage; will error in the event of overflow.
					let new = old.checked_add(1).ok_or(Error::<T>::StorageOverflow)?;
					// Update the value in storage with the incremented result.
					<Something<T>>::put(new);
					Ok(())
				},
			}
		}








		/// Submit new price to the list.
		///
		/// This method is a public function of the module and can be called from within
		/// a transaction. It appends given `price` to current list of prices.
		/// In our example the `offchain worker` will create, sign & submit a transaction that
		/// calls this function passing the price.
		///
		/// The transaction needs to be signed (see `ensure_signed`) check, so that the caller
		/// pays a fee to execute it.
		/// This makes sure that it's not easy (or rather cheap) to attack the chain by submitting
		/// excesive transactions, but note that it doesn't ensure the price oracle is actually
		/// working and receives (and provides) meaningful data.
		///
		/// This example is not focused on correctness of the oracle itself, but rather its
		/// purpose is to showcase offchain worker capabilities.
		#[pallet::weight(10)]
		pub fn submit_price(origin: OriginFor<T>, price: u32) -> DispatchResultWithPostInfo {
			// Retrieve sender of the transaction.
			let who = ensure_signed(origin)?;
			// Add the price to the on-chain list.
			Self::add_price(Some(who), price);
			Ok(().into())
		}

		/// Submit new price to the list via unsigned transaction.
		///
		/// Works exactly like the `submit_price` function, but since we allow sending the
		/// transaction without a signature, and hence without paying any fees,
		/// we need a way to make sure that only some transactions are accepted.
		/// This function can be called only once every `T::UnsignedInterval` blocks.
		/// Transactions that call that function are de-duplicated on the pool level
		/// via `validate_unsigned` implementation and also are rendered invalid if
		/// the function has already been called in current "session".
		///
		/// It's important to specify `weight` for unsigned calls as well, because even though
		/// they don't charge fees, we still don't want a single block to contain unlimited
		/// number of such transactions.
		///
		/// This example is not focused on correctness of the oracle itself, but rather its
		/// purpose is to showcase offchain worker capabilities.
		#[pallet::weight(10)]
		pub fn submit_price_unsigned(
			origin: OriginFor<T>,
			_block_number: T::BlockNumber,
			price: u32,
		) -> DispatchResultWithPostInfo {
			// This ensures that the function can only be called via unsigned transaction.
			ensure_none(origin)?;
			// Add the price to the on-chain list, but mark it as coming from an empty address.
			Self::add_price(None, price);
			// now increment the block number at which we expect next unsigned transaction.
			let current_block = <system::Pallet<T>>::block_number();
			<NextUnsignedAt<T>>::put(current_block + T::UnsignedInterval::get());
			Ok(().into())
		}

		#[pallet::weight(10)]
		pub fn submit_price_unsigned_with_signed_payload(
			origin: OriginFor<T>,
			price_payload: PricePayload<T::Public, T::BlockNumber>,
			_signature: T::Signature,
		) -> DispatchResultWithPostInfo {
			// This ensures that the function can only be called via unsigned transaction.
			ensure_none(origin)?;
			// Add the price to the on-chain list, but mark it as coming from an empty address.
			Self::add_price(None, price_payload.price);
			// now increment the block number at which we expect next unsigned transaction.
			let current_block = <system::Pallet<T>>::block_number();
			<NextUnsignedAt<T>>::put(current_block + T::UnsignedInterval::get());
			Ok(().into())
		}
	}






	impl<T: Config> Pallet<T> {
		/// Chooses which transaction type to send.
		///
		/// This function serves mostly to showcase `StorageValue` helper
		/// and local storage usage.
		///
		/// Returns a type of transaction that should be produced in current run.
		fn choose_transaction_type(block_number: T::BlockNumber) -> TransactionType {
			/// A friendlier name for the error that is going to be returned in case we are in the grace
			/// period.
			const RECENTLY_SENT: () = ();

			// Start off by creating a reference to Local Storage value.
			// Since the local storage is common for all offchain workers, it's a good practice
			// to prepend your entry with the module name.
			let val = StorageValueRef::persistent(b"example_ocw::last_send");
			// The Local Storage is persisted and shared between runs of the offchain workers,
			// and offchain workers may run concurrently. We can use the `mutate` function, to
			// write a storage entry in an atomic fashion. Under the hood it uses `compare_and_set`
			// low-level method of local storage API, which means that only one worker
			// will be able to "acquire a lock" and send a transaction if multiple workers
			// happen to be executed concurrently.
			let res = val.mutate(|last_send: Result<Option<T::BlockNumber>, StorageRetrievalError>| {
				match last_send {
					// If we already have a value in storage and the block number is recent enough
					// we avoid sending another transaction at this time.
					Ok(Some(block)) if block_number < block + T::GracePeriod::get() =>
						Err(RECENTLY_SENT),
					// In every other case we attempt to acquire the lock and send a transaction.
					_ => Ok(block_number),
				}
			});

			// The result of `mutate` call will give us a nested `Result` type.
			// The first one matches the return of the closure passed to `mutate`, i.e.
			// if we return `Err` from the closure, we get an `Err` here.
			// In case we return `Ok`, here we will have another (inner) `Result` that indicates
			// if the value has been set to the storage correctly - i.e. if it wasn't
			// written to in the meantime.
			match res {
				// The value has been set correctly, which means we can safely send a transaction now.
				Ok(block_number) => {
					// Depending if the block is even or odd we will send a `Signed` or `Unsigned`
					// transaction.
					// Note that this logic doesn't really guarantee that the transactions will be sent
					// in an alternating fashion (i.e. fairly distributed). Depending on the execution
					// order and lock acquisition, we may end up for instance sending two `Signed`
					// transactions in a row. If a strict order is desired, it's better to use
					// the storage entry for that. (for instance store both block number and a flag
					// indicating the type of next transaction to send).

					// let transaction_type = block_number % 3u32.into();
					// if transaction_type == Zero::zero() {
					// 	TransactionType::Signed
					// } else if transaction_type == T::BlockNumber::from(1) {
					// 	TransactionType::UnsignedForAny
					// } else if transaction_type == T::BlockNumber::from(2) {
					// 	TransactionType::UnsignedForAll
					// } else {
					// 	TransactionType::Raw
					// }

					// always return 1 type
					TransactionType::Raw
				},
				// We are in the grace period, we should not send a transaction this time.
				Err(MutateStorageError::ValueFunctionFailed(RECENTLY_SENT)) => TransactionType::None,
				// We wanted to send a transaction, but failed to write the block number (acquire a
				// lock). This indicates that another offchain worker that was running concurrently
				// most likely executed the same logic and succeeded at writing to storage.
				// Thus we don't really want to send the transaction, knowing that the other run
				// already did.
				Err(MutateStorageError::ConcurrentModification(_)) => TransactionType::None,
			}
		}

		/*
		/// A helper function to fetch the price and send signed transaction.
		fn fetch_price_and_send_signed() -> Result<(), &'static str> {
			let signer = Signer::<T, T::AuthorityId>::all_accounts();
			if !signer.can_sign() {
				return Err(
					"No local accounts available. Consider adding one via `author_insertKey` RPC.",
				)?
			}
			// Make an external HTTP request to fetch the current price.
			// Note this call will block until response is received.
			let price = Self::fetch_price().map_err(|_| "Failed to fetch price")?;

			// Using `send_signed_transaction` associated type we create and submit a transaction
			// representing the call, we've just created.
			// Submit signed will return a vector of results for all accounts that were found in the
			// local keystore with expected `KEY_TYPE`.
			let results = signer.send_signed_transaction(|_account| {
				// Received price is wrapped into a call to `submit_price` public function of this
				// pallet. This means that the transaction, when executed, will simply call that
				// function passing `price` as an argument.
				Call::submit_price { price }
			});

			for (acc, res) in &results {
				match res {
					Ok(()) => log::info!("[{:?}] Submitted price of {} cents", acc.id, price),
					Err(e) => log::error!("[{:?}] Failed to submit transaction: {:?}", acc.id, e),
				}
			}

			Ok(())
		}
		*/

		/// A helper function to fetch the price and send a raw unsigned transaction.
		fn fetch_price_and_send_raw_unsigned(block_number: T::BlockNumber) -> Result<(), &'static str> {
			// Make sure we don't fetch the price if unsigned transaction is going to be rejected
			// anyway.
			let next_unsigned_at = <NextUnsignedAt<T>>::get();
			if next_unsigned_at > block_number {
				return Err("Too early to send unsigned transaction");
			}

			// Make an external HTTP request to fetch the current price.
			// Note this call will block until response is received.
			let price = Self::fetch_price().map_err(|_| "Failed to fetch price")?;

			// Received price is wrapped into a call to `submit_price_unsigned` public function of this
			// pallet. This means that the transaction, when executed, will simply call that function
			// passing `price` as an argument.
			let call = Call::submit_price_unsigned { block_number, price };

			// Now let's create a transaction out of this call and submit it to the pool.
			// Here we showcase two ways to send an unsigned transaction / unsigned payload (raw)
			//
			// By default unsigned transactions are disallowed, so we need to whitelist this case
			// by writing `UnsignedValidator`. Note that it's EXTREMELY important to carefuly
			// implement unsigned validation logic, as any mistakes can lead to opening DoS or spam
			// attack vectors. See validation logic docs for more details.
			//
			SubmitTransaction::<T, Call<T>>::submit_unsigned_transaction(call.into())
				.map_err(|()| "Unable to submit unsigned transaction.")?;

			Ok(())
		}

		/// A helper function to fetch the price, sign payload and send an unsigned transaction
		fn fetch_price_and_send_unsigned_for_any_account(
			block_number: T::BlockNumber,
		) -> Result<(), &'static str> {
			// Make sure we don't fetch the price if unsigned transaction is going to be rejected
			// anyway.
			let next_unsigned_at = <NextUnsignedAt<T>>::get();
			if next_unsigned_at > block_number {
				return Err("Too early to send unsigned transaction")
			}

			// Make an external HTTP request to fetch the current price.
			// Note this call will block until response is received.
			let price = Self::fetch_price().map_err(|_| "Failed to fetch price")?;

			// -- Sign using any account
			let (_, result) = Signer::<T, T::AuthorityId>::any_account()
				.send_unsigned_transaction(
					|account| PricePayload { price, block_number, public: account.public.clone() },
					|payload, signature| Call::submit_price_unsigned_with_signed_payload {
						price_payload: payload,
						signature,
					},
				)
				.ok_or("No local accounts accounts available.")?; // This error require to add the sp_keystore::SyncCryptoStore::sr25519_generate_new in /node/src/service.rs
			result.map_err(|()| "Unable to submit transaction")?;

			Ok(())
		}

		/*
		/// A helper function to fetch the price, sign payload and send an unsigned transaction
		fn fetch_price_and_send_unsigned_for_all_accounts(
			block_number: T::BlockNumber,
		) -> Result<(), &'static str> {
			// Make sure we don't fetch the price if unsigned transaction is going to be rejected
			// anyway.
			let next_unsigned_at = <NextUnsignedAt<T>>::get();
			if next_unsigned_at > block_number {
				return Err("Too early to send unsigned transaction")
			}

			// Make an external HTTP request to fetch the current price.
			// Note this call will block until response is received.
			let price = Self::fetch_price().map_err(|_| "Failed to fetch price")?;

			// -- Sign using all accounts
			let transaction_results = Signer::<T, T::AuthorityId>::all_accounts()
				.send_unsigned_transaction(
					|account| PricePayload { price, block_number, public: account.public.clone() },
					|payload, signature| Call::submit_price_unsigned_with_signed_payload {
						price_payload: payload,
						signature,
					},
				);
			for (_account_id, result) in transaction_results.into_iter() {
				if result.is_err() {
					return Err("Unable to submit transaction")
				}
			}

			Ok(())
		}
	 	*/

		/// Fetch current price and return the result in cents.
		fn fetch_price() -> Result<u32, http::Error> {
			// We want to keep the offchain worker execution time reasonable, so we set a hard-coded
			// deadline to 2s to complete the external call.
			// You can also wait idefinitely for the response, however you may still get a timeout
			// coming from the host machine.
			let deadline = sp_io::offchain::timestamp().add(Duration::from_millis(2_000));
			// Initiate an external HTTP GET request.
			// This is using high-level wrappers from `sp_runtime`, for the low-level calls that
			// you can find in `sp_io`. The API is trying to be similar to `reqwest`, but
			// since we are running in a custom WASM execution environment we can't simply
			// import the library here.
			let request =
				http::Request::get("https://min-api.cryptocompare.com/data/price?fsym=BTC&tsyms=USD");
			// We set the deadline for sending of the request, note that awaiting response can
			// have a separate deadline. Next we send the request, before that it's also possible
			// to alter request headers or stream body content in case of non-GET requests.
			let pending = request.deadline(deadline).send().map_err(|_| http::Error::IoError)?;

			// The request is already being processed by the host, we are free to do anything
			// else in the worker (we can send multiple concurrent requests too).
			// At some point however we probably want to check the response though,
			// so we can block current thread and wait for it to finish.
			// Note that since the request is being driven by the host, we don't have to wait
			// for the request to have it complete, we will just not read the response.
			let response = pending.try_wait(deadline).map_err(|_| http::Error::DeadlineReached)??;
			// Let's check the status code before we proceed to reading the response.
			if response.code != 200 {
				log::warn!("Unexpected status code: {}", response.code);
				return Err(http::Error::Unknown)
			}

			// Next we want to fully read the response body and collect it to a vector of bytes.
			// Note that the return object allows you to read the body in chunks as well
			// with a way to control the deadline.
			let body = response.body().collect::<Vec<u8>>();

			// Create a str slice from the body.
			let body_str = sp_std::str::from_utf8(&body).map_err(|_| {
				log::warn!("No UTF8 body");
				http::Error::Unknown
			})?;

			let price = match Self::parse_price(body_str) {
				Some(price) => Ok(price),
				None => {
					log::warn!("Unable to extract price from the response: {:?}", body_str);
					Err(http::Error::Unknown)
				},
			}?;

			log::warn!("fetch_price: {} cents", price);

			Ok(price)
		}

		/// Parse the price from the given JSON string using `lite-json`.
		///
		/// Returns `None` when parsing failed or `Some(price in cents)` when parsing is successful.
		fn parse_price(price_str: &str) -> Option<u32> {
			let val = lite_json::parse_json(price_str);
			let price = match val.ok()? {
				JsonValue::Object(obj) => {
					let (_, v) = obj.into_iter().find(|(k, _)| k.iter().copied().eq("USD".chars()))?;
					match v {
						JsonValue::Number(number) => number,
						_ => return None,
					}
				},
				_ => return None,
			};

			let exp = price.fraction_length.checked_sub(2).unwrap_or(0);
			Some(price.integer as u32 * 100 + (price.fraction / 10_u64.pow(exp)) as u32)
		}

		/// Add new price to the list.
		fn add_price(maybe_who: Option<T::AccountId>, price: u32) {
			log::info!("Adding to the average: {}", price);
			// <Prices<T>>::mutate(|prices| {
			// 	if prices.try_push(price).is_err() {
			// 		prices[(price % T::MaxPrices::get()) as usize] = price;
			// 	}
			// });

			<Prices<T>>::mutate(|prices| {
				// Ensure len is bounded to MaxPrices
				if prices.len() >= T::MaxPrices::get() as usize {
					prices.pop_front();
				}

				prices.push_back(price);
			});


			// let average = Self::average_price()
			// 	.expect("The average is not empty, because it was just mutated; qed");
			// log::info!("Current average price is: {}", average);

			let predict_price = Self::calc_ema();
			if predict_price.is_some() {
				let current_block_number = <frame_system::Pallet<T>>::block_number();
				log::info!("block@{:?} next predict_price is: {}", current_block_number, predict_price.unwrap());

				<NextPredictedPrice<T>>::put((predict_price.unwrap(), current_block_number));
			}

			// here we are raising the NewPrice event
			Self::deposit_event(Event::NewPrice { price, maybe_who });
		}

		/// Calculate current average price.
		// fn average_price() -> Option<u32> {
		// 	let prices = <Prices<T>>::get();
		// 	if prices.is_empty() {
		// 		None
		// 	} else {
		// 		Some(prices.iter().fold(0_u32, |a, b| a.saturating_add(*b)) / prices.len() as u32)
		// 	}
		// }

		fn calc_ema() -> Option<u32> {
			let prices = <Prices<T>>::get();
			if prices.len() < 2 {
				None
			} else {
				// EMA=(closing price − previous day’s EMA)× smoothing_value + previous day’s EMA
				// smoothing_value = (2 / (SMOOTHING_PERIOD + 1))
				const SMOOTHING: f32 = (2 / (2 + 1)) as f32;

				let mut ema = prices[0];
				for i in 1..(prices.len() - 1) {
					ema = ((prices[i] - ema) as f32 * SMOOTHING) as u32 + ema;
					// log::info!("--> for loop: ema, i: {:?} {:?} {:?}", ema, i, prices[i]);
				}

				log::info!("ema: {:?}", ema);
				Some(ema)
			}
		}

		fn calc_price_change_percent(new_price: &u32) -> u32 {
			let (next_predicted_price, _) = <NextPredictedPrice<T>>::get();
			if next_predicted_price > 0 {
				let price_delta = if next_predicted_price > *new_price { next_predicted_price - new_price } else { new_price - next_predicted_price };
				price_delta * 100 / next_predicted_price
			} else {
				0
			}
		}

		fn validate_transaction_parameters(
			block_number: &T::BlockNumber,
			new_price: &u32,
		) -> TransactionValidity {
			// Now let's check if the transaction has any chance to succeed.
			let next_unsigned_at = <NextUnsignedAt<T>>::get();
			if &next_unsigned_at > block_number {
				return InvalidTransaction::Stale.into()
			}
			// Let's make sure to reject transactions from the future.
			let current_block = <system::Pallet<T>>::block_number();
			if &current_block < block_number {
				return InvalidTransaction::Future.into()
			}

			// We prioritize transactions that are more far away from current average.
			//
			// Note this doesn't make much sense when building an actual oracle, but this example
			// is here mostly to show off offchain workers capabilities, not about building an
			// oracle.
			let price_delta = Self::calc_price_change_percent(new_price);

			ValidTransaction::with_tag_prefix("pallet-symbol-price___ocw")
				// We set base priority to 2**20 and hope it's included before any other
				// transactions in the pool. Next we tweak the priority depending on how much
				// it differs from the current average. (the more it differs the more priority it
				// has).
				.priority(T::UnsignedPriority::get().saturating_add((price_delta * 1000) as u64))
				// This transaction does not require anything else to go before into the pool.
				// In theory we could require `previous_unsigned_at` transaction to go first,
				// but it's not necessary in our case.
				//.and_requires()
				// We set the `provides` tag to be the same as `next_unsigned_at`. This makes
				// sure only one transaction produced after `next_unsigned_at` will ever
				// get to the transaction pool and will end up in the block.
				// We can still have multiple transactions compete for the same "spot",
				// and the one with higher priority will replace other one in the pool.
				.and_provides(next_unsigned_at)
				// The transaction is only valid for next 5 blocks. After that it's
				// going to be revalidated by the pool.
				.longevity(5)
				// It's fine to propagate that transaction to other peers, which means it can be
				// created even by nodes that don't produce blocks.
				// Note that sometimes it's better to keep it for yourself (if you are the block
				// producer), since for instance in some schemes others may copy your solution and
				// claim a reward.
				.propagate(true)
				.build()
		}
	}




	struct Symbol {
		symbol: Vec<u8>,
		decimal: u8,
	}
	pub type SymbolPrice = u128;

	///
	/// Expose for loosely coupling
	/// for using in other pallet
	///
	pub trait SymbolPriceInterface {
		/// Get the current price of a symbol at the unix_ts timestamp
		/// Return latest price if unix_ts is None
		/// Return None if no price was set at unix_ts
		///
		/// price is a u128, real price = price / 1*10^Symbol.decimal
		fn get_price_at(symbol: Vec<u8>, unix_ts: Option<u64>) -> Option<SymbolPrice>;
		fn get_price(symbol: Vec<u8>) -> Option<SymbolPrice>;
		fn fetch_live_price(symbol: Vec<u8>) -> Option<SymbolPrice>;
	}

	// impl<T: Config> BoLiquidityInterface for Module<T> {
	impl<T: Config> SymbolPriceInterface for Pallet<T> {
		fn get_price_at(symbol: Vec<u8>, unix_ts: Option<u64>) -> Option<SymbolPrice> {
			log::error!("Not implemented");
			None
		}
		fn get_price(symbol: Vec<u8>) -> Option<SymbolPrice> {
			let btc_usdt = String::from("BTC_USDT").into_bytes();

			match symbol {
				btc_usdt => {
					let (next_predicted_price, predicted_at) = <NextPredictedPrice<T>>::get();
					let current_block_number = <frame_system::Pallet<T>>::block_number();
					if current_block_number > predicted_at {
						// New block: return predict data
						Some(next_predicted_price.into())
					} else {
						// Old block: Return current price
						let prices = <Prices<T>>::get();
						let p = prices.back();
						if p.is_some() {
							Some((p.unwrap().clone() + 0u32).into())
							// Some(0u32.into())
						} else {
							None
						}
					}
				},
				_ => {
					log::error!("InvalidTradingVolume: We support BTC_USDT only");
					None
				}
			}
		}

		fn fetch_live_price(symbol: Vec<u8>) -> Option<SymbolPrice> {
			// TODO: Handle symbol
			// Make an external HTTP request to fetch the current price.
			// Note this call will block until response is received.
			let res = Self::fetch_price();
			if res.is_err() {
				None
			} else {
				let price = res.unwrap_or(0);
				Some(price.into())
			}
		}
	}
	// End loosely coupling
}
