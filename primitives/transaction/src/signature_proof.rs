use std::{cmp::Ord, error::Error, fmt, str};

use base64::prelude::{Engine, BASE64_URL_SAFE_NO_PAD};
use bitflags::bitflags;
use nimiq_hash::{Blake2bHash, Hash, HashOutput, Sha256Hash};
use nimiq_keys::{Address, Ed25519PublicKey, Ed25519Signature, PublicKey, Signature};
use nimiq_primitives::policy::Policy;
use nimiq_serde::{Deserialize, Serialize, SerializedMaxSize};
use nimiq_utils::merkle::{Blake2bMerklePath, PoWBlake2bMerklePath};
use url::Url;

#[derive(Clone, Debug)]
pub struct SignatureProof {
    pub public_key: PublicKey,
    pub merkle_path: Blake2bMerklePath,
    pub signature: Signature,
    pub webauthn_fields: Option<WebauthnExtraFields>,
}

impl SerializedMaxSize for SignatureProof {
    #[allow(clippy::identity_op)]
    const MAX_SIZE: usize = 0
        + PublicKey::MAX_SIZE
        + Policy::MAX_MERKLE_PATH_SIZE
        + Signature::MAX_SIZE
        + nimiq_serde::option_max_size(WebauthnExtraFields::MAX_SIZE);
}

impl SignatureProof {
    pub fn from(
        public_key: PublicKey,
        signature: Signature,
        webauthn_fields: Option<WebauthnExtraFields>,
    ) -> Self {
        SignatureProof {
            public_key,
            merkle_path: Blake2bMerklePath::empty(),
            signature,
            webauthn_fields,
        }
    }

    pub fn from_ed25519(public_key: Ed25519PublicKey, signature: Ed25519Signature) -> Self {
        SignatureProof {
            public_key: PublicKey::Ed25519(public_key),
            merkle_path: Blake2bMerklePath::empty(),
            signature: Signature::Ed25519(signature),
            webauthn_fields: None,
        }
    }

    pub fn try_from_webauthn(
        public_key: PublicKey,
        merkle_path: Option<Blake2bMerklePath>,
        signature: Signature,
        authenticator_data: &[u8],
        client_data_json: &str,
    ) -> Result<Self, SerializationError> {
        if authenticator_data.len() < 32 {
            return Err(SerializationError::new("authenticator data too short"));
        }

        let webauthn_fields = WebauthnExtraFields::from_client_data_json(
            client_data_json,
            authenticator_data[32..].into(),
        )?;

        let rp_id: Sha256Hash = webauthn_fields.rp_id()?;

        if *rp_id.as_bytes() != authenticator_data[0..32] {
            return Err(SerializationError::new(
                "computed RP ID does not match authenticator data",
            ));
        }

        Ok(SignatureProof {
            public_key,
            merkle_path: merkle_path.unwrap_or_default(),
            signature,
            webauthn_fields: Some(webauthn_fields),
        })
    }

    pub fn compute_signer(&self) -> Address {
        let merkle_root = match self.public_key {
            PublicKey::Ed25519(ref public_key) => self.merkle_path.compute_root(public_key),
            PublicKey::ES256(ref public_key) => self.merkle_path.compute_root(public_key),
        };
        Address::from(merkle_root)
    }

    pub fn is_signed_by(&self, address: &Address) -> bool {
        self.compute_signer() == *address
    }

    pub fn verify(&self, message: &[u8]) -> bool {
        if self.webauthn_fields.is_some() {
            self.verify_webauthn(message)
        } else {
            self.verify_signature(message)
        }
    }

    fn verify_webauthn(&self, message: &[u8]) -> bool {
        let webauthn_fields = self
            .webauthn_fields
            .as_ref()
            .expect("Webauthn fields not set");

        // 1. We need to hash the message to get our challenge data
        let challenge: Blake2bHash = message.hash();

        // 2. The RP ID is the SHA256 hash of the hostname
        let rp_id = match webauthn_fields.rp_id() {
            Ok(rp_id) => rp_id,
            Err(error) => {
                debug!(%error, "Failed to extract RP ID");
                return false;
            }
        };

        // 3. Build the authenticatorData from the RP ID and the suffix
        let mut authenticator_data = Vec::new();
        authenticator_data.extend_from_slice(rp_id.as_slice());
        authenticator_data.extend_from_slice(&webauthn_fields.authenticator_data_suffix);

        // 4. Build the clientDataJSON from challenge and origin
        let json = webauthn_fields.to_client_data_json(challenge.as_slice());

        // Hash the clientDataJSON
        let client_data_hash: Sha256Hash = json.hash();

        // 5. Concat authenticatorData and clientDataHash to build the data signed by Webauthn
        let mut signed_data = authenticator_data;
        signed_data.extend_from_slice(client_data_hash.as_slice());

        self.verify_signature(&signed_data)
    }

    fn verify_signature(&self, message: &[u8]) -> bool {
        match self.public_key {
            PublicKey::Ed25519(ref public_key) => match self.signature {
                Signature::Ed25519(ref signature) => public_key.verify(signature, message),
                _ => false,
            },
            PublicKey::ES256(ref public_key) => match self.signature {
                Signature::ES256(ref signature) => public_key.verify(signature, message),
                _ => false,
            },
        }
    }

    pub fn make_type_and_flags_byte(&self) -> u8 {
        // Use the lower 4 bits for the algorithm variant
        let mut type_flags = match self.public_key {
            PublicKey::Ed25519(_) => SignatureProofAlgorithm::Ed25519,
            PublicKey::ES256(_) => SignatureProofAlgorithm::ES256,
        } as u8;

        // Use the upper 4 bits as flags
        let mut flags = SignatureProofFlags::default();
        if self.webauthn_fields.is_some() {
            flags.insert(SignatureProofFlags::WEBAUTHN_FIELDS);
        }
        type_flags |= flags.bits() << 4;

        type_flags
    }

    pub fn parse_type_and_flags_byte(
        byte: u8,
    ) -> Result<(SignatureProofAlgorithm, SignatureProofFlags), String> {
        // The algorithm is encoded in the lower 4 bits
        let type_byte = byte & 0b0000_1111;
        let algorithm = match type_byte {
            0 => SignatureProofAlgorithm::Ed25519,
            1 => SignatureProofAlgorithm::ES256,
            _ => return Err(format!("Invalid signature proof algorithm: {}", type_byte)),
        };
        // The flags are encoded in the upper 4 bits
        let flags = SignatureProofFlags::from_bits_truncate(byte >> 4);

        Ok((algorithm, flags))
    }
}

impl Default for SignatureProof {
    /// Default to Ed25519 public key and signature without Webauthn fields for backwards compatibility
    fn default() -> Self {
        SignatureProof {
            public_key: PublicKey::Ed25519(Default::default()),
            merkle_path: Default::default(),
            signature: Signature::Ed25519(Default::default()),
            webauthn_fields: None,
        }
    }
}

#[repr(u8)]
pub enum SignatureProofAlgorithm {
    Ed25519,
    ES256,
}

bitflags! {
    #[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
    /// Store flags for serialized signature proofs. Can only use 4 bits,
    /// because the flags are stored in the upper 4 bits of the `type` field.
    pub struct SignatureProofFlags: u8 {
        const WEBAUTHN_FIELDS = 1 << 0;
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
    /// Some authenticators may behave non-standard for signing with Webauthn:
    ///
    /// - they might not include the mandatory `crossOrigin` field in clientDataJSON
    /// - they might escape the `origin`'s forward slashes with backslashes, although not necessary for UTF-8 nor JSON encoding
    ///
    /// To allow the WebauthnSignatureProof to construct a correct `clientDataJSON` for verification,
    /// the proof needs to know these non-standard behaviors.
    ///
    /// See this tracking issue for Android Chrome: https://bugs.chromium.org/p/chromium/issues/detail?id=1233616
    pub struct WebauthnClientDataFlags: u8 {
        const NO_CROSSORIGIN_FIELD  = 1 << 0;
        const ESCAPED_ORIGIN_SLASHES = 1 << 1;

        // const HAS_EXTRA_FIELDS = 1 << 7; // TODO Replace client_data_extra_fields length null byte when no extra fields are present
    }
}

/// Extra data needed to verify a webauthn signature.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebauthnExtraFields {
    /// The `origin` field, but weirdly stored.
    ///
    /// Everything in the `origin` field in the `clientDataJSON`, exactly as it appears in the
    /// JSON, not undoing any JSON escaping etc.
    pub origin_json_str: String,
    /// Does the `clientDataJSON` have a `"crossOrigin":false` field?
    pub has_cross_origin_field: bool,
    /// Extra, unknown fields in the `clientDataJSON`.
    ///
    /// Exactly as it appears in the JSON, including the leading comma, excluding the final closing
    /// brace.
    pub client_data_extra_json: String,
    /// Extra data included in the signed data.
    ///
    /// It's between the RP ID and the `clientDataHash`.
    pub authenticator_data_suffix: Vec<u8>,
}

impl SerializedMaxSize for WebauthnExtraFields {
    const MAX_SIZE: usize = Policy::MAX_SUPPORTED_WEB_AUTH_SIZE;
}

impl WebauthnExtraFields {
    pub fn from_client_data_json(
        client_data_json: &str,
        authenticator_data_suffix: Vec<u8>,
    ) -> Result<WebauthnExtraFields, SerializationError> {
        let rest = client_data_json;

        let rest = rest
            .strip_prefix(r#"{"type":"webauthn.get","challenge":""#)
            .ok_or_else(|| SerializationError::new("invalid clientDataJson prefix"))?;

        let (challenge_base64_json_str, rest) = rest
            .split_once('"')
            .ok_or_else(|| SerializationError::new("invalid challenge"))?;

        if challenge_base64_json_str.as_bytes().last().copied() == Some(b'\\') {
            return Err(SerializationError::new(
                "challenge can't contain escaped quotes",
            ));
        }

        let challenge = BASE64_URL_SAFE_NO_PAD
            .decode(challenge_base64_json_str)
            .map_err(|e| SerializationError::new(&format!("invalid challenge base64: {e}")))?;

        if BASE64_URL_SAFE_NO_PAD.encode(&challenge) != challenge_base64_json_str {
            return Err(SerializationError::new("non-canonical challenge base64"));
        }

        let rest = rest
            .strip_prefix(r#","origin":""#)
            .ok_or_else(|| SerializationError::new("couldn't find origin field"))?;

        let (origin_json_str, rest) = rest
            .split_once('"')
            .ok_or_else(|| SerializationError::new("invalid origin"))?;

        if origin_json_str.as_bytes().last().copied() == Some(b'\\') {
            return Err(SerializationError::new(
                "origin can't contain escaped quotes",
            ));
        }

        let has_cross_origin_field;
        let mut rest = rest;
        if let Some(r) = rest.strip_prefix(r#","crossOrigin":"#) {
            has_cross_origin_field = true;
            if let Some(r) = r.strip_prefix("false") {
                rest = r;
            } else {
                return Err(SerializationError::new("crossOrigin must be false"));
            }
        } else {
            has_cross_origin_field = false;
        }

        if rest.as_bytes().last().copied() != Some(b'}') {
            return Err(SerializationError::new("invalid clientDataJSON suffix"));
        }

        let client_data_extra_json = &rest[..rest.len() - 1];

        let result = WebauthnExtraFields {
            origin_json_str: origin_json_str.into(),
            has_cross_origin_field,
            client_data_extra_json: client_data_extra_json.into(),
            authenticator_data_suffix,
        };
        assert_eq!(result.to_client_data_json(&challenge), client_data_json);
        Ok(result)
    }
    fn to_client_data_json(&self, challenge: &[u8]) -> String {
        let challenge_base64 = BASE64_URL_SAFE_NO_PAD.encode(challenge);
        let origin_json_str = &self.origin_json_str;
        let cross_origin_json = if self.has_cross_origin_field {
            r#","crossOrigin":false"#
        } else {
            ""
        };
        let client_data_extra_json = &self.client_data_extra_json;
        format!(
            r#"{{"type":"webauthn.get","challenge":"{challenge_base64}","origin":"{origin_json_str}"{cross_origin_json}{client_data_extra_json}}}"#
        )
    }
    fn rp_id(&self) -> Result<Sha256Hash, SerializationError> {
        let origin_json_str = &self.origin_json_str;
        let origin: Url = serde_json::from_str(&format!("\"{origin_json_str}\""))
            .map_err(|e| SerializationError::new(&format!("invalid origin URL: {e}")))?;
        let hostname = origin
            .host_str()
            .ok_or_else(|| SerializationError::new("invalid origin URL: missing hostname"))?;
        Ok(hostname.hash())
    }
}

/// This struct represents signature proofs in the Proof-of-Work chain. The difference to proofs on the
/// Albatross chain are that PoW signature could only be of type Ed25519 (they had no type-and-flags byte)
/// and that the merkle path had a different serialization.
#[derive(Clone, Debug, Deserialize)]
pub struct PoWSignatureProof {
    pub public_key: Ed25519PublicKey,
    pub merkle_path: PoWBlake2bMerklePath,
    pub signature: Ed25519Signature,
}

impl PoWSignatureProof {
    pub fn verify(&self, message: &[u8]) -> bool {
        self.public_key.verify(&self.signature, message)
    }

    pub fn into_pos(self) -> SignatureProof {
        SignatureProof {
            public_key: PublicKey::Ed25519(self.public_key),
            merkle_path: self.merkle_path.into_pos(),
            signature: Signature::Ed25519(self.signature),
            webauthn_fields: None,
        }
    }
}

#[test]
fn it_can_correctly_deserialize_pow_signature_proof() {
    let bin = hex::decode("08600ec9f0d44dc8d43275c705d7780caa31497d2620da4d7838d10574a6dfa100410b82decb73b7c6f4047b4fb504000c364edd9a3337e5194b60f896d31904ccab8bf310cf808fd98a9b3b13096b6701d53bbba8402465d08cb99948c8407500")
        .unwrap();
    let _ = PoWSignatureProof::deserialize_all(&bin).unwrap();
}

#[test]
fn it_can_correctly_deserialize_pow_multisig_signature_proof() {
    let bin = hex::decode("c79090f344bf7ed4cdd6c25512ee61d1d5fe9cff643263342996ba3448df189f0280de8d7ee7e54f301095294d494024430c8b251b4ebf9b1384922dc7f9dd24422f830e231d26cdc3bbd1f55f1918757568522acae62c21e8046190ea84d6e8ff160caadca71723067d5080d6c3858b61ef8cdf286326818e90ddbefe23af2d529cef7654be5b99bb418786d49e164b24f9db1c482545d8e4473804a53b889e4b07")
        .unwrap();
    let _ = PoWSignatureProof::deserialize_all(&bin).unwrap();
}

mod serde_derive {
    use std::fmt;

    use nimiq_keys::{
        ES256PublicKey, ES256Signature, Ed25519PublicKey, Ed25519Signature, PublicKey, Signature,
    };
    use serde::ser::SerializeStruct;

    use super::*;

    const STRUCT_NAME: &str = "SignatureProof";

    const FIELDS: &[&str] = &[
        "type_and_flags",
        "public_key",
        "merkle_path",
        "signature",
        "webauthn_fields",
    ];

    impl serde::Serialize for SignatureProof {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let mut length = 4; // type field (algorithm & flags), public key, merkle path, signature
            if self.webauthn_fields.is_some() {
                length += 1; // Webauthn fields
            }

            let mut state = serializer.serialize_struct(STRUCT_NAME, length)?;

            state.serialize_field(FIELDS[0], &self.make_type_and_flags_byte())?;

            // Serialize public key without enum variant, as that is already encoded in the `type`/`algorithm` field
            match self.public_key {
                PublicKey::Ed25519(ref public_key) => {
                    state.serialize_field(FIELDS[1], public_key)?;
                }
                PublicKey::ES256(ref public_key) => {
                    state.serialize_field(FIELDS[1], public_key)?;
                }
            }

            // Serialize merkle path as is
            state.serialize_field(FIELDS[2], &self.merkle_path)?;

            // Serialize signature without enum variant, as that is already encoded in the `type`/`algorithm` field
            match self.signature {
                Signature::Ed25519(ref signature) => {
                    state.serialize_field(FIELDS[3], signature)?;
                }
                Signature::ES256(ref signature) => {
                    state.serialize_field(FIELDS[3], signature)?;
                }
            }

            // When present, serialize webauthn fields flattened into the root struct. The option variant is
            // encoded in the `type` field.
            if self.webauthn_fields.is_some() {
                state.serialize_field(FIELDS[4], self.webauthn_fields.as_ref().unwrap())?;
            }

            state.end()
        }
    }

    struct SignatureProofVisitor;

    impl<'de> serde::Deserialize<'de> for SignatureProof {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_struct(STRUCT_NAME, FIELDS, SignatureProofVisitor)
        }
    }

    impl<'de> serde::de::Visitor<'de> for SignatureProofVisitor {
        type Value = SignatureProof;

        fn expecting(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
            write!(f, "a SignatureProof")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<SignatureProof, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let type_field: u8 = seq
                .next_element()?
                .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;

            let (algorithm, flags) = SignatureProof::parse_type_and_flags_byte(type_field)
                .map_err(serde::de::Error::custom)?;

            let public_key = match algorithm {
                SignatureProofAlgorithm::Ed25519 => {
                    let public_key: Ed25519PublicKey = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
                    PublicKey::Ed25519(public_key)
                }
                SignatureProofAlgorithm::ES256 => {
                    let public_key: ES256PublicKey = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
                    PublicKey::ES256(public_key)
                }
            };

            let merkle_path: Blake2bMerklePath = seq
                .next_element()?
                .ok_or_else(|| serde::de::Error::invalid_length(2, &self))?;

            if merkle_path.serialized_size() > Policy::MAX_MERKLE_PATH_SIZE {
                return Err(serde::de::Error::invalid_length(2, &self));
            }

            let signature = match algorithm {
                SignatureProofAlgorithm::Ed25519 => {
                    let signature: Ed25519Signature = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(3, &self))?;
                    Signature::Ed25519(signature)
                }
                SignatureProofAlgorithm::ES256 => {
                    let signature: ES256Signature = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(3, &self))?;
                    Signature::ES256(signature)
                }
            };

            let webauthn_fields = if flags.contains(SignatureProofFlags::WEBAUTHN_FIELDS) {
                Some(
                    seq.next_element::<WebauthnExtraFields>()?
                        .ok_or_else(|| serde::de::Error::invalid_length(4, &self))?,
                )
            } else {
                None
            };

            if webauthn_fields.serialized_size() > WebauthnExtraFields::MAX_SIZE {
                return Err(serde::de::Error::invalid_length(4, &self));
            }

            Ok(SignatureProof {
                public_key,
                merkle_path,
                signature,
                webauthn_fields,
            })
        }
    }
}

#[derive(Debug)]
pub struct SerializationError {
    msg: String,
}

impl SerializationError {
    fn new(msg: &str) -> SerializationError {
        SerializationError {
            msg: msg.to_string(),
        }
    }
}

impl fmt::Display for SerializationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl Error for SerializationError {
    fn description(&self) -> &str {
        &self.msg
    }
}

impl From<url::ParseError> for SerializationError {
    fn from(err: url::ParseError) -> Self {
        SerializationError::new(&format!("Failed to parse URL: {}", err))
    }
}
