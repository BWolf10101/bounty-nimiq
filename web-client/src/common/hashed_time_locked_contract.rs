use nimiq_keys::{PublicKey, Signature};
use nimiq_serde::Deserialize;
use nimiq_transaction::account::htlc_contract::{
    AnyHash, CreationTransactionData, OutgoingHTLCTransactionProof,
};
use wasm_bindgen::prelude::*;

use crate::common::transaction::{
    PlainHtlcData, PlainHtlcEarlyResolveProof, PlainHtlcRegularTransferProof,
    PlainHtlcTimeoutResolveProof, PlainTransactionProof, PlainTransactionRecipientData,
};
#[cfg(feature = "primitives")]
use crate::common::transaction::{PlainTransactionProofType, PlainTransactionRecipientDataType};

#[wasm_bindgen]
pub struct HashedTimeLockedContract;

#[cfg(feature = "primitives")]
#[wasm_bindgen]
impl HashedTimeLockedContract {
    #[wasm_bindgen(js_name = dataToPlain)]
    pub fn data_to_plain(data: &[u8]) -> Result<PlainTransactionRecipientDataType, JsError> {
        let plain = HashedTimeLockedContract::parse_data(data)?;
        Ok(serde_wasm_bindgen::to_value(&plain)?.into())
    }

    #[wasm_bindgen(js_name = proofToPlain)]
    pub fn proof_to_plain(proof: &[u8]) -> Result<PlainTransactionProofType, JsError> {
        let plain = HashedTimeLockedContract::parse_proof(proof)?;
        Ok(serde_wasm_bindgen::to_value(&plain)?.into())
    }
}

impl HashedTimeLockedContract {
    pub fn parse_data(bytes: &[u8]) -> Result<PlainTransactionRecipientData, JsError> {
        let data = CreationTransactionData::deserialize_all(bytes)?;

        Ok(PlainTransactionRecipientData::Htlc(PlainHtlcData {
            raw: hex::encode(bytes),
            sender: data.sender.to_user_friendly_address(),
            recipient: data.recipient.to_user_friendly_address(),
            hash_algorithm: match data.hash_root {
                AnyHash::Blake2b(_) => "blake2b".to_string(),
                AnyHash::Sha256(_) => "sha256".to_string(),
                AnyHash::Sha512(_) => "sha512".to_string(),
            },
            hash_root: data.hash_root.to_hex(),
            hash_count: data.hash_count,
            timeout: data.timeout,
        }))
    }

    pub fn parse_proof(bytes: &[u8]) -> Result<PlainTransactionProof, JsError> {
        let proof = OutgoingHTLCTransactionProof::deserialize_all(bytes)?;

        Ok(match proof {
            OutgoingHTLCTransactionProof::RegularTransfer {
                hash_depth,
                hash_root,
                pre_image,
                signature_proof,
            } => PlainTransactionProof::RegularTransfer(PlainHtlcRegularTransferProof {
                raw: hex::encode(bytes),
                hash_algorithm: match hash_root {
                    AnyHash::Blake2b(_) => "blake2b".to_string(),
                    AnyHash::Sha256(_) => "sha256".to_string(),
                    AnyHash::Sha512(_) => "sha512".to_string(),
                },
                hash_depth,
                hash_root: hash_root.to_hex(),
                pre_image: pre_image.to_hex(),
                signer: signature_proof.compute_signer().to_user_friendly_address(),
                signature: match signature_proof.signature {
                    Signature::Ed25519(ref signature) => signature.to_hex(),
                    Signature::ES256(ref signature) => signature.to_hex(),
                },
                public_key: match signature_proof.public_key {
                    PublicKey::Ed25519(ref public_key) => public_key.to_hex(),
                    PublicKey::ES256(ref public_key) => public_key.to_hex(),
                },
                path_length: signature_proof.merkle_path.len() as u8,
            }),
            OutgoingHTLCTransactionProof::TimeoutResolve {
                signature_proof_sender,
            } => PlainTransactionProof::TimeoutResolve(PlainHtlcTimeoutResolveProof {
                raw: hex::encode(bytes),
                creator: signature_proof_sender
                    .compute_signer()
                    .to_user_friendly_address(),
                creator_signature: match signature_proof_sender.signature {
                    Signature::Ed25519(ref signature) => signature.to_hex(),
                    Signature::ES256(ref signature) => signature.to_hex(),
                },
                creator_public_key: match signature_proof_sender.public_key {
                    PublicKey::Ed25519(ref public_key) => public_key.to_hex(),
                    PublicKey::ES256(ref public_key) => public_key.to_hex(),
                },
                creator_path_length: signature_proof_sender.merkle_path.len() as u8,
            }),
            OutgoingHTLCTransactionProof::EarlyResolve {
                signature_proof_recipient,
                signature_proof_sender,
            } => PlainTransactionProof::EarlyResolve(PlainHtlcEarlyResolveProof {
                raw: hex::encode(bytes),
                signer: signature_proof_recipient
                    .compute_signer()
                    .to_user_friendly_address(),
                signature: match signature_proof_recipient.signature {
                    Signature::Ed25519(ref signature) => signature.to_hex(),
                    Signature::ES256(ref signature) => signature.to_hex(),
                },
                public_key: match signature_proof_recipient.public_key {
                    PublicKey::Ed25519(ref public_key) => public_key.to_hex(),
                    PublicKey::ES256(ref public_key) => public_key.to_hex(),
                },
                path_length: signature_proof_recipient.merkle_path.len() as u8,
                creator: signature_proof_sender
                    .compute_signer()
                    .to_user_friendly_address(),
                creator_signature: match signature_proof_sender.signature {
                    Signature::Ed25519(ref signature) => signature.to_hex(),
                    Signature::ES256(ref signature) => signature.to_hex(),
                },
                creator_public_key: match signature_proof_sender.public_key {
                    PublicKey::Ed25519(ref public_key) => public_key.to_hex(),
                    PublicKey::ES256(ref public_key) => public_key.to_hex(),
                },
                creator_path_length: signature_proof_sender.merkle_path.len() as u8,
            }),
        })
    }
}
