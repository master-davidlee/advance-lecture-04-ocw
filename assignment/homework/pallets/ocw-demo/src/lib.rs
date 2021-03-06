//! A demonstration of an offchain worker that sends onchain callbacks



/*我使用了未签名交易带payload的方式，因为我觉得价格数据有以下两个特点
        1. 是实时变动的数据，需要实时更新。
        2. 需要有提交价格的账户信息来防止提供假数据。
  基于这两个特点，如果用签名交易手续费会很多，如果用未签名交易又没法防止提交假数据，所以采用了带payload的方式
  我将价格保留到小数点后3位，并且为了方便存储都乘了1000变成整数，所以使用时需要除1000才是真实的USD价格 */
  
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(test)]
mod tests;

use core::{convert::TryInto, fmt};
use frame_support::{
    debug, decl_error, decl_event, decl_module, decl_storage, dispatch::DispatchResult,
};
use parity_scale_codec::{Decode, Encode};

use frame_system::{
    self as system, ensure_none, ensure_signed,
    offchain::{
        AppCrypto, CreateSignedTransaction, SendSignedTransaction, SendUnsignedTransaction,
        SignedPayload, Signer, SigningTypes, SubmitTransaction,
    },
};
use sp_core::crypto::KeyTypeId;
use sp_runtime::{
    offchain as rt_offchain,
    offchain::{
        storage::StorageValueRef,
        storage_lock::{BlockAndTime, StorageLock},
    },
    transaction_validity::{
        InvalidTransaction, TransactionSource, TransactionValidity, ValidTransaction,
    },
    RuntimeDebug,
};
use sp_std::{collections::vec_deque::VecDeque, prelude::*, str};

use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

/// Defines application identifier for crypto keys of this module.
///
/// Every module that deals with signatures needs to declare its unique identifier for
/// its crypto keys.
/// When an offchain worker is signing transactions it's going to request keys from type
/// `KeyTypeId` via the keystore to sign the transaction.
/// The keys can be inserted manually via RPC (see `author_insertKey`).
pub const KEY_TYPE: KeyTypeId = KeyTypeId(*b"demo");
pub const NUM_VEC_LEN: usize = 10;
/// The type to sign and send transactions.
pub const UNSIGNED_TXS_PRIORITY: u64 = 100;

// We are fetching information from the github public API about organization`substrate-developer-hub`.
pub const HTTP_REMOTE_REQUEST: &str = "https://api.coincap.io/v2/assets/polkadot";


pub const FETCH_TIMEOUT_PERIOD: u64 = 3000; // in milli-seconds
pub const LOCK_TIMEOUT_EXPIRATION: u64 = FETCH_TIMEOUT_PERIOD + 1000; // in milli-seconds
pub const LOCK_BLOCK_EXPIRATION: u32 = 3; // in block number

/// Based on the above `KeyTypeId` we need to generate a pallet-specific crypto type wrapper.
/// We can utilize the supported crypto kinds (`sr25519`, `ed25519` and `ecdsa`) and augment
/// them with the pallet-specific identifier.
pub mod crypto {
    use crate::KEY_TYPE;
    use sp_core::sr25519::Signature as Sr25519Signature;
    use sp_runtime::app_crypto::{app_crypto, sr25519};
    use sp_runtime::{traits::Verify, MultiSignature, MultiSigner};

    app_crypto!(sr25519, KEY_TYPE);

    pub struct TestAuthId;
    // implemented for ocw-runtime
    impl frame_system::offchain::AppCrypto<MultiSigner, MultiSignature> for TestAuthId {
        type RuntimeAppPublic = Public;
        type GenericSignature = sp_core::sr25519::Signature;
        type GenericPublic = sp_core::sr25519::Public;
    }

    // implemented for mock runtime in test
    impl frame_system::offchain::AppCrypto<<Sr25519Signature as Verify>::Signer, Sr25519Signature>
        for TestAuthId
    {
        type RuntimeAppPublic = Public;
        type GenericSignature = sp_core::sr25519::Signature;
        type GenericPublic = sp_core::sr25519::Public;
    }
}

#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug)]
pub struct Payload<Public> {
    price: u32,
    public: Public,
}

impl<T: SigningTypes> SignedPayload<T> for Payload<T::Public> {
    fn public(&self) -> T::Public {
        self.public.clone()
    }
}

#[derive(Deserialize, Encode, Decode, Default)]
struct Price {
    #[serde(deserialize_with = "de_string_to_bytes")]
    priceUsd: Vec<u8>,
}

pub fn de_string_to_bytes<'de, D>(de: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    
    let s: &str = Deserialize::deserialize(de)?;
    
    Ok(s.as_bytes().to_vec())
}

/// This is the pallet's configuration trait
pub trait Trait: system::Trait + CreateSignedTransaction<Call<Self>> {
    /// The identifier type for an offchain worker.
    type AuthorityId: AppCrypto<Self::Public, Self::Signature>;
    /// The overarching dispatch call type.
    type Call: From<Call<Self>>;
    /// The overarching event type.
    type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;
}

decl_storage! {
    trait Store for Module<T: Trait> as Example {
        /// A vector of recently submitted numbers. Bounded by NUM_VEC_LEN
        

        Prices get(fn prices): VecDeque<u32>;
    }
}

decl_event!(
    /// Events generated by the module.
    pub enum Event<T>
    where
        AccountId = <T as system::Trait>::AccountId,
    {
        /// Event generated when a new number is accepted to contribute to the average.
        NewNumber(Option<AccountId>, u32),
    }
);

decl_error! {
    pub enum Error for Module<T: Trait> {
        // Error returned when not sure which ocw function to executed
        UnknownOffchainMux,

        // Error returned when making signed transactions in off-chain worker
        NoLocalAcctForSigning,
        
        // Error returned when making unsigned transactions with signed payloads in off-chain worker
        OffchainUnsignedTxSignedPayloadError,

        // Error returned when fetching github info
        HttpFetchingError,

        ParseError,

        WrongRespondCode,
        StringToJsonError,
        BytestoStringError,
        GetbytesError,
        PendingError,
        DoubleResultOneError,
        DoubleResultTwoError,
    }
}

decl_module! {
    pub struct Module<T: Trait> for enum Call where origin: T::Origin {
        fn deposit_event() = default;

        

        #[weight = 10000]
        pub fn submit_price_unsigned_with_signed_payload(origin, payload: Payload<T::Public>,
            _signature: T::Signature) -> DispatchResult
        {
            let _ = ensure_none(origin)?;

            // we don't need to verify the signature here because it has been verified in
            //   `validate_unsigned` function when sending out the unsigned tx.
            let Payload { price, public } = payload;
            debug::info!("submit_price_unsigned_with_signed_payload: ({}, {:?})", price, public);
            Self::append_or_replace_price(price);

            Self::deposit_event(RawEvent::NewNumber(None, price));
            Ok(())
        }

        fn offchain_worker(block_number: T::BlockNumber) {
            debug::info!("Entering off-chain worker");

            // Here we are showcasing various techniques used when running off-chain workers (ocw)
            // 1. Sending signed transaction from ocw
            // 2. Sending unsigned transaction from ocw
            // 3. Sending unsigned transactions with signed payloads from ocw
            // 4. Fetching JSON via http requests in ocw
            // const TX_TYPES: u32 = 3;
            // let modu = block_number.try_into().map_or(TX_TYPES, |bn: u32| bn % TX_TYPES);
            // let result = match modu {
            // 	0 => Self::offchain_signed_tx(block_number),
            // 	1 => Self::offchain_unsigned_tx(block_number),
            // 	2 => Self::offchain_unsigned_tx_signed_payload(block_number),

            // 	_ => Err(Error::<T>::UnknownOffchainMux),
            // };
            let result = Self::offchain_unsigned_tx_signed_payload(block_number);

            if let Err(e) = result {
                debug::error!("offchain_worker error: {:?}", e);
            }
        }
    }
}

impl<T: Trait> Module<T> {
    /// Append a new number to the tail of the list, removing an element from the head if reaching
    ///   the bounded length.
    

    fn append_or_replace_price(price: u32) {
        Prices::mutate(|prices| {
            if prices.len() == NUM_VEC_LEN {
                let _ = prices.pop_front();
            }
            prices.push_back(price);
            debug::info!("Number vector: {:?}", prices);
        });
    }

    /// Fetch from remote and deserialize the JSON to a struct
    fn parse_get_price_struct() -> Result<Price, Error<T>> {
        let resp_bytes = Self::fetch_from_remote().map_err(|e| {
            debug::error!("get_bytes error: {:?}", e);
            <Error<T>>::GetbytesError
        })?;

        let resp_str = str::from_utf8(&resp_bytes).map_err(|_| <Error<T>>::BytestoStringError)?;
        // Print out our fetched JSON string
        debug::info!("from_get_price_struct_function {}", resp_str);

        // Deserializing JSON to struct, thanks to `serde` and `serde_derive`
        let resp_json: Value = serde_json::from_str(&resp_str).map_err(|e| {
            debug::error!("String to json Error {}", e);
            <Error<T>>::StringToJsonError
        })?;

        let data_str = serde_json::to_string(&resp_json["data"]).map_err(|e| {
            debug::error!("data_str error {}", e);
            <Error<T>>::StringToJsonError
        })?;
        
        let price: Price = serde_json::from_str(&data_str).map_err(|e| {
            debug::error!("get price struct error {}", e);
            <Error<T>>::StringToJsonError
        })?;

        Ok(price)
    }

    /// This function uses the `offchain::http` API to query the remote github information,
    ///   and returns the JSON response as vector of bytes.
    fn fetch_from_remote() -> Result<Vec<u8>, Error<T>> {
        debug::info!("sending request to: {}", HTTP_REMOTE_REQUEST);

        // Initiate an external HTTP GET request. This is using high-level wrappers from `sp_runtime`.
        let request = rt_offchain::http::Request::get(HTTP_REMOTE_REQUEST);

        // Keeping the offchain worker execution time reasonable, so limiting the call to be within 3s.
        let timeout = sp_io::offchain::timestamp()
            .add(rt_offchain::Duration::from_millis(FETCH_TIMEOUT_PERIOD));

        // For github API request, we also need to specify `user-agent` in http request header.
        //   See: https://developer.github.com/v3/#user-agent-required
        let pending = request
            .deadline(timeout) // Setting the timeout time
            .send() // Sending the request out by the host
            .map_err(|_| <Error<T>>::PendingError)?;

        // By default, the http request is async from the runtime perspective. So we are asking the
        //   runtime to wait here.
        // The returning value here is a `Result` of `Result`, so we are unwrapping it twice by two `?`
        //   ref: https://substrate.dev/rustdocs/v2.0.0/sp_runtime/offchain/http/struct.PendingRequest.html#method.try_wait
        let response = pending
            .try_wait(timeout)
            .map_err(|_| <Error<T>>::DoubleResultOneError)?
            .map_err(|_| <Error<T>>::DoubleResultTwoError)?;

        if response.code != 200 {
            debug::error!("Unexpected http request status code: {}", response.code);
            return Err(<Error<T>>::WrongRespondCode);
        }
        debug::info!("got success code 200");
        // Next we fully read the response body and collect it to a vector of bytes.
        Ok(response.body().collect::<Vec<u8>>())
    }

   

    

    fn offchain_unsigned_tx_signed_payload(block_number: T::BlockNumber) -> Result<(), Error<T>> {
        // Retrieve the signer to sign the payload
        let signer = Signer::<T, T::AuthorityId>::any_account();
        let price_struct = Self::parse_get_price_struct().map_err(|e| {
            debug::error!("unsigned_tx_signed_payload error: {:?}", e);
            <Error<T>>::HttpFetchingError
        })?;
        let price_u32 = Self::vec_to_u32(price_struct.priceUsd).map_err(|e| {
            debug::error!("ParseError: {:?}", e);
            <Error<T>>::ParseError
        })?;
        Self::append_or_replace_price(price_u32);
        if let Some((_, res)) = signer.send_unsigned_transaction(
            |acct| Payload {
                price: price_u32,
                public: acct.public.clone(),
            },
            Call::submit_price_unsigned_with_signed_payload,
        ) {
            return res.map_err(|_| {
                debug::error!("Failed in offchain_unsigned_tx_signed_payload");
                <Error<T>>::OffchainUnsignedTxSignedPayloadError
            });
        }

        // The case of `None`: no account is available for sending
        debug::error!("No local account available");
        Err(<Error<T>>::NoLocalAcctForSigning)
    }

    fn vec_to_u32(val_u8: Vec<u8>) -> Result<u32, Error<T>> {
        // Convert to number
        let val_f32: f32 = core::str::from_utf8(&val_u8)
            .map_err(|_| Error::<T>::ParseError)?
            .parse::<f32>()
            .map_err(|_| Error::<T>::ParseError)?;
        Ok((val_f32 * 10000.) as u32)
    }
}

impl<T: Trait> frame_support::unsigned::ValidateUnsigned for Module<T> {
    type Call = Call<T>;

    fn validate_unsigned(_source: TransactionSource, call: &Self::Call) -> TransactionValidity {
        let valid_tx = |provide| {
            ValidTransaction::with_tag_prefix("ocw-demo")
                .priority(UNSIGNED_TXS_PRIORITY)
                .and_provides([&provide])
                .longevity(3)
                .propagate(true)
                .build()
        };

        match call {
            
            Call::submit_price_unsigned_with_signed_payload(ref payload, ref signature) => {
                if !SignedPayload::<T>::verify::<T::AuthorityId>(payload, signature.clone()) {
                    return InvalidTransaction::BadProof.into();
                }
                valid_tx(b"submit_price_unsigned_with_signed_payload".to_vec())
            }
            _ => InvalidTransaction::Call.into(),
        }
    }
}


