use aes::cipher::generic_array::GenericArray;
use aes::cipher::KeyInit;
use aes::Aes256;
use aes_kw::Kek;
use argon2::Argon2;
use rand::RngExt;
use sha2::{Digest, Sha256};
use xts_mode::{get_tweak_default, Xts128};

use crate::YakError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// AES-256-XTS key length: two AES-256 keys (32 + 32 = 64 bytes).
const MASTER_KEY_LEN: usize = 64;

/// Salt length for Argon2id.
const SALT_LEN: usize = 16;

/// Bytes derived from Argon2id: 32-byte KEK + 32-byte verification material.
const KDF_OUTPUT_LEN: usize = 64;

/// AES-256-KW wraps a 64-byte master key into 72 bytes (8-byte integrity overhead).
const WRAPPED_KEY_LEN: usize = MASTER_KEY_LEN + 8;

/// SHA-256 output length for password verification.
const VERIFICATION_HASH_LEN: usize = 32;

/// On-disk size of the encryption config (everything after the encrypted_flag byte).
/// salt(16) + m_cost(4) + t_cost(4) + p_cost(1) + verification_hash(32) + wrapped_key(72) = 129
pub(crate) const ENCRYPTION_CONFIG_SIZE: usize =
    SALT_LEN + 4 + 4 + 1 + VERIFICATION_HASH_LEN + WRAPPED_KEY_LEN;

// Default Argon2id parameters (OWASP recommended baseline).
const DEFAULT_M_COST: u32 = 19456; // ~19 MiB memory
const DEFAULT_T_COST: u32 = 2; // 2 iterations
const DEFAULT_P_COST: u32 = 1; // single-lane

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Encryption parameters stored in the L2 header.
pub(crate) struct EncryptionConfig {
    pub salt: [u8; SALT_LEN],
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub verification_hash: [u8; VERIFICATION_HASH_LEN],
    pub wrapped_key: [u8; WRAPPED_KEY_LEN],
}

/// Runtime cipher for encrypting/decrypting blocks.
/// Holds the XTS cipher instance, initialized from the 64-byte master key.
/// `Xts128<Aes256>` stores only the AES key schedule (no mutable state)
/// and its encrypt/decrypt methods take `&self`, so this is Send + Sync.
pub(crate) struct BlockCipher {
    cipher: Xts128<Aes256>,
}

impl std::fmt::Debug for BlockCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockCipher").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Key derivation
// ---------------------------------------------------------------------------

/// Derive KEK (32 bytes) and verification material (32 bytes) from a password and salt.
fn derive_key_material(
    password: &[u8],
    salt: &[u8; SALT_LEN],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<[u8; KDF_OUTPUT_LEN], YakError> {
    let params = argon2::Params::new(m_cost, t_cost, p_cost, Some(KDF_OUTPUT_LEN))
        .map_err(|e| YakError::IoError(format!("argon2id params: {}", e)))?;
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

    let mut output = [0u8; KDF_OUTPUT_LEN];
    argon2
        .hash_password_into(password, salt, &mut output)
        .map_err(|e| YakError::IoError(format!("argon2id derivation failed: {}", e)))?;
    Ok(output)
}

/// Build a BlockCipher from a 64-byte master key.
fn cipher_from_master_key(master_key: &[u8; MASTER_KEY_LEN]) -> BlockCipher {
    let cipher = Xts128::<Aes256>::new(
        Aes256::new(GenericArray::from_slice(&master_key[..32])),
        Aes256::new(GenericArray::from_slice(&master_key[32..])),
    );
    BlockCipher { cipher }
}

// ---------------------------------------------------------------------------
// Public (crate) API
// ---------------------------------------------------------------------------

/// Create encryption config and cipher for a new encrypted file.
///
/// Generates a random salt and master key, derives KEK from password via
/// Argon2id, wraps the master key with AES-256-KW, and computes a SHA-256
/// verification hash of extra derived bytes.
pub(crate) fn create_encryption(
    password: &[u8],
) -> Result<(EncryptionConfig, BlockCipher), YakError> {
    // Generate random salt (rand 0.8+ is cryptographically secure - uses ChaCha12Rng seeded with OsRng)
    let mut salt = [0u8; SALT_LEN];
    rand::rng().fill(&mut salt);

    // Generate random master key
    let mut master_key = [0u8; MASTER_KEY_LEN];
    rand::rng().fill(&mut master_key);

    // Derive KEK + verification material
    let derived = derive_key_material(
        password,
        &salt,
        DEFAULT_M_COST,
        DEFAULT_T_COST,
        DEFAULT_P_COST,
    )?;
    let kek_bytes: &[u8; 32] = derived[..32].try_into().unwrap();
    let verification_material = &derived[32..64];

    // Compute verification hash
    let verification_hash: [u8; VERIFICATION_HASH_LEN] =
        Sha256::digest(verification_material).into();

    // Wrap master key with AES-256-KW
    let kek = Kek::from(*kek_bytes);
    let mut wrapped_key = [0u8; WRAPPED_KEY_LEN];
    kek.wrap(&master_key, &mut wrapped_key)
        .map_err(|e| YakError::IoError(format!("AES-KW wrap failed: {:?}", e)))?;

    let config = EncryptionConfig {
        salt,
        m_cost: DEFAULT_M_COST,
        t_cost: DEFAULT_T_COST,
        p_cost: DEFAULT_P_COST,
        verification_hash,
        wrapped_key,
    };

    let cipher = cipher_from_master_key(&master_key);
    Ok((config, cipher))
}

/// Verify password and unwrap master key from an existing encryption config.
///
/// Derives key material from password + stored salt/params, verifies the
/// SHA-256 hash, then unwraps the master key via AES-256-KW.
pub(crate) fn open_encryption(
    config: &EncryptionConfig,
    password: &[u8],
) -> Result<BlockCipher, YakError> {
    let derived = derive_key_material(
        password,
        &config.salt,
        config.m_cost,
        config.t_cost,
        config.p_cost,
    )?;
    let kek_bytes: &[u8; 32] = derived[..32].try_into().unwrap();
    let verification_material = &derived[32..64];

    // Verify password via SHA-256 hash comparison
    let computed_hash: [u8; VERIFICATION_HASH_LEN] = Sha256::digest(verification_material).into();
    if computed_hash != config.verification_hash {
        return Err(YakError::WrongPassword(
            "password verification hash mismatch".to_string(),
        ));
    }

    // Unwrap master key
    let kek = Kek::from(*kek_bytes);
    let mut master_key = [0u8; MASTER_KEY_LEN];
    kek.unwrap(&config.wrapped_key, &mut master_key)
        .map_err(|_| YakError::WrongPassword("AES-KW unwrap failed".to_string()))?;

    Ok(cipher_from_master_key(&master_key))
}

/// Encrypt a single block in place. The tweak is the block index (zero-extended to u128).
pub(crate) fn encrypt_block(cipher: &BlockCipher, block_index: u64, data: &mut [u8]) {
    let tweak = get_tweak_default(block_index as u128);
    cipher.cipher.encrypt_sector(data, tweak);
}

/// Decrypt a single block in place. The tweak is the block index (zero-extended to u128).
pub(crate) fn decrypt_block(cipher: &BlockCipher, block_index: u64, data: &mut [u8]) {
    let tweak = get_tweak_default(block_index as u128);
    cipher.cipher.decrypt_sector(data, tweak);
}

/// Encrypt a contiguous run of blocks in place. Each block_size chunk gets
/// its own tweak derived from `first_block_index + i`.
pub(crate) fn encrypt_blocks(
    cipher: &BlockCipher,
    data: &mut [u8],
    block_size: usize,
    first_block_index: u64,
) {
    cipher.cipher.encrypt_area(
        data,
        block_size,
        first_block_index as u128,
        get_tweak_default,
    );
}

/// Decrypt a contiguous run of blocks in place. Each block_size chunk gets
/// its own tweak derived from `first_block_index + i`.
pub(crate) fn decrypt_blocks(
    cipher: &BlockCipher,
    data: &mut [u8],
    block_size: usize,
    first_block_index: u64,
) {
    cipher.cipher.decrypt_area(
        data,
        block_size,
        first_block_index as u128,
        get_tweak_default,
    );
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

/// Serialize an EncryptionConfig to bytes for storage in the L2 header.
/// Layout: salt(16) | m_cost(4) | t_cost(4) | p_cost_u8(1) | verification_hash(32) | wrapped_key(72)
pub(crate) fn serialize_config(config: &EncryptionConfig) -> [u8; ENCRYPTION_CONFIG_SIZE] {
    let mut buf = [0u8; ENCRYPTION_CONFIG_SIZE];
    let mut pos = 0;

    buf[pos..pos + SALT_LEN].copy_from_slice(&config.salt);
    pos += SALT_LEN;

    buf[pos..pos + 4].copy_from_slice(&config.m_cost.to_le_bytes());
    pos += 4;

    buf[pos..pos + 4].copy_from_slice(&config.t_cost.to_le_bytes());
    pos += 4;

    // p_cost is stored as a single byte (values above 255 are impractical)
    buf[pos] = config.p_cost as u8;
    pos += 1;

    buf[pos..pos + VERIFICATION_HASH_LEN].copy_from_slice(&config.verification_hash);
    pos += VERIFICATION_HASH_LEN;

    buf[pos..pos + WRAPPED_KEY_LEN].copy_from_slice(&config.wrapped_key);

    buf
}

/// Deserialize an EncryptionConfig from L2 header bytes.
pub(crate) fn deserialize_config(data: &[u8]) -> Result<EncryptionConfig, YakError> {
    if data.len() < ENCRYPTION_CONFIG_SIZE {
        return Err(YakError::IoError(format!(
            "encryption config too short: {} < {}",
            data.len(),
            ENCRYPTION_CONFIG_SIZE
        )));
    }

    let mut pos = 0;

    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&data[pos..pos + SALT_LEN]);
    pos += SALT_LEN;

    let m_cost = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
    pos += 4;

    let t_cost = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
    pos += 4;

    let p_cost = data[pos] as u32;
    pos += 1;

    let mut verification_hash = [0u8; VERIFICATION_HASH_LEN];
    verification_hash.copy_from_slice(&data[pos..pos + VERIFICATION_HASH_LEN]);
    pos += VERIFICATION_HASH_LEN;

    let mut wrapped_key = [0u8; WRAPPED_KEY_LEN];
    wrapped_key.copy_from_slice(&data[pos..pos + WRAPPED_KEY_LEN]);

    Ok(EncryptionConfig {
        salt,
        m_cost,
        t_cost,
        p_cost,
        verification_hash,
        wrapped_key,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PASSWORD: &[u8] = b"test-password-123";

    // Use fast Argon2id params for tests (default params are too slow for unit tests).
    // Tests that need fast params call create_encryption_with_params directly.
    fn create_test_encryption(
        password: &[u8],
    ) -> Result<(EncryptionConfig, BlockCipher), YakError> {
        let mut salt = [0u8; SALT_LEN];
        rand::rng().fill(&mut salt);

        let mut master_key = [0u8; MASTER_KEY_LEN];
        rand::rng().fill(&mut master_key);

        // Fast params for testing
        let m_cost = 256; // 256 KiB — minimal for Argon2id
        let t_cost = 1;
        let p_cost = 1;

        let derived = derive_key_material(password, &salt, m_cost, t_cost, p_cost)?;
        let kek_bytes: &[u8; 32] = derived[..32].try_into().unwrap();
        let verification_material = &derived[32..64];

        let verification_hash: [u8; VERIFICATION_HASH_LEN] =
            Sha256::digest(verification_material).into();

        let kek = Kek::from(*kek_bytes);
        let mut wrapped_key = [0u8; WRAPPED_KEY_LEN];
        kek.wrap(&master_key, &mut wrapped_key)
            .map_err(|e| YakError::IoError(format!("AES-KW wrap failed: {:?}", e)))?;

        let config = EncryptionConfig {
            salt,
            m_cost,
            t_cost,
            p_cost,
            verification_hash,
            wrapped_key,
        };

        let cipher = cipher_from_master_key(&master_key);
        Ok((config, cipher))
    }

    fn open_test_encryption(
        config: &EncryptionConfig,
        password: &[u8],
    ) -> Result<BlockCipher, YakError> {
        open_encryption(config, password)
    }

    #[test]
    fn round_trip_create_open() {
        let (config, _cipher) = create_test_encryption(TEST_PASSWORD).unwrap();
        let _cipher2 = open_test_encryption(&config, TEST_PASSWORD).unwrap();
    }

    #[test]
    fn wrong_password_fails() {
        let (config, _cipher) = create_test_encryption(TEST_PASSWORD).unwrap();
        let result = open_test_encryption(&config, b"wrong-password");
        assert!(result.is_err());
        match result.unwrap_err() {
            YakError::WrongPassword(_) => {}
            other => panic!("expected WrongPassword, got: {:?}", other),
        }
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let (_, cipher) = create_test_encryption(TEST_PASSWORD).unwrap();

        let original =
            b"Hello, encrypted world! This is a test block of at least 16 bytes for AES.";
        let mut data = original.to_vec();

        encrypt_block(&cipher, 42, &mut data);
        // Ciphertext should differ from plaintext
        assert_ne!(&data[..], &original[..]);

        decrypt_block(&cipher, 42, &mut data);
        assert_eq!(&data[..], &original[..]);
    }

    #[test]
    fn different_tweaks_produce_different_ciphertext() {
        let (_, cipher) = create_test_encryption(TEST_PASSWORD).unwrap();

        let original = [0xABu8; 64];
        let mut data_a = original;
        let mut data_b = original;

        encrypt_block(&cipher, 0, &mut data_a);
        encrypt_block(&cipher, 1, &mut data_b);

        // Same plaintext, different block indices → different ciphertext
        assert_ne!(data_a, data_b);
    }

    #[test]
    fn config_serialization_round_trip() {
        let (config, _) = create_test_encryption(TEST_PASSWORD).unwrap();
        let bytes = serialize_config(&config);
        assert_eq!(bytes.len(), ENCRYPTION_CONFIG_SIZE);

        let restored = deserialize_config(&bytes).unwrap();
        assert_eq!(restored.salt, config.salt);
        assert_eq!(restored.m_cost, config.m_cost);
        assert_eq!(restored.t_cost, config.t_cost);
        assert_eq!(restored.p_cost, config.p_cost);
        assert_eq!(restored.verification_hash, config.verification_hash);
        assert_eq!(restored.wrapped_key, config.wrapped_key);
    }

    #[test]
    fn deserialization_rejects_short_data() {
        let result = deserialize_config(&[0u8; 10]);
        assert!(result.is_err());
    }
}
