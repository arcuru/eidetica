# Security Best Practices

This document outlines security patterns and practices used throughout the Eidetica codebase, focusing on authentication, authorization, cryptographic operations, and secure data handling.

## Core Security Architecture

### 1. **Authentication System**

Eidetica uses Ed25519 digital signatures for all entry authentication:

**Key Components**:

- **Ed25519 Cryptography**: High-performance, secure signature scheme
- **Content-Addressable Entries**: Tampering detection through hash verification
- **Signature Verification**: All entries must be signed by authorized keys
- **Key Management**: Separate storage of private keys outside synchronized data

### 2. **Authorization Model**

Hierarchical permission system with fine-grained access control:

```rust
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    Read,
    Write,
    Admin,
}

impl Permission {
    /// Check if current permission level allows the required operation
    pub fn allows(&self, required: &Permission) -> bool {
        self >= required
    }
}
```

**Permission Hierarchy**:

- **Read**: Can read data and compute states
- **Write**: Can create and modify data entries
- **Admin**: Can manage permissions and authentication settings

### 3. **Secure Entry Creation**

All entries must be authenticated during creation:

```rust
impl AtomicOp {
    /// Commit operation with authentication
    pub fn commit(self) -> Result<Entry> {
        let entry_builder = self.into_inner()?;

        // Verify authentication key exists and has permissions
        self.verify_authentication_key()?;

        // Create entry with signature
        let unsigned_entry = entry_builder.build_unsigned()?;
        let signed_entry = self.sign_entry(unsigned_entry)?;

        // Store with verification
        self.backend.store_verified(&signed_entry)?;

        Ok(signed_entry)
    }
}
```

## Cryptographic Best Practices

### 1. **Digital Signature Handling**

Use Ed25519 signatures for all entry authentication:

```rust
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey, SigningKey};

pub struct EntrySignature {
    key_name: String,
    signature: Option<Signature>,
}

impl EntrySignature {
    /// Sign entry with private key
    pub fn sign_entry(entry: &Entry, private_key: &SigningKey) -> Result<Signature> {
        let canonical_bytes = entry.to_canonical_bytes()?;
        let signature = private_key.sign(&canonical_bytes);
        Ok(signature)
    }

    /// Verify entry signature
    pub fn verify_signature(
        entry: &Entry,
        signature: &Signature,
        public_key: &VerifyingKey
    ) -> Result<()> {
        let canonical_bytes = entry.to_canonical_bytes()?;
        public_key.verify(&canonical_bytes, signature)
            .map_err(|_| AuthError::InvalidSignature {
                key_name: entry.signature().key_name.clone()
            })?;
        Ok(())
    }
}
```

### 2. **Key Generation and Storage**

Secure key generation and storage patterns:

```rust
use rand::rngs::OsRng;

pub struct KeyManager {
    private_keys: HashMap<String, SigningKey>,
    public_keys: HashMap<String, VerifyingKey>,
}

impl KeyManager {
    /// Generate new Ed25519 keypair
    pub fn generate_keypair(&mut self, key_name: String) -> Result<VerifyingKey> {
        let mut csprng = OsRng;  // Cryptographically secure random number generator
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();

        // Store keys securely
        self.private_keys.insert(key_name.clone(), signing_key);
        self.public_keys.insert(key_name, verifying_key);

        Ok(verifying_key)
    }

    /// Securely clear private key from memory
    pub fn remove_private_key(&mut self, key_name: &str) {
        if let Some(mut key) = self.private_keys.remove(key_name) {
            // Zero out key material (implementation dependent)
            secure_zero(&mut key);
        }
    }
}

// Platform-specific secure memory clearing
fn secure_zero(data: &mut [u8]) {
    // Use platform-specific secure memory clearing
    // This prevents compiler optimizations from removing the zeroing
    #[cfg(target_os = "linux")]
    unsafe {
        std::ptr::write_volatile(data.as_mut_ptr(), 0);
    }

    // TODO: Implement for other platforms
}
```

### 3. **Canonical Serialization**

Ensure consistent serialization for signature verification:

```rust
impl Entry {
    /// Create canonical byte representation for signing
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>> {
        // Create entry copy without signature for signing
        let mut unsigned_entry = self.clone();
        unsigned_entry.signature = EntrySignature {
            key_name: self.signature.key_name.clone(),
            signature: None,  // Remove signature for canonical form
        };

        // Sort all fields deterministically
        unsigned_entry.sort_for_canonical_form();

        // Serialize to canonical JSON
        let canonical_json = serde_json::to_string(&unsigned_entry)
            .map_err(|e| DataError::SerializationFailed {
                reason: e.to_string()
            })?;

        Ok(canonical_json.into_bytes())
    }

    fn sort_for_canonical_form(&mut self) {
        // Sort parent vectors
        self.tree.parents.sort();

        for subtree in &mut self.subtrees {
            subtree.parents.sort();
        }

        // Sort subtrees vector
        self.subtrees.sort_by(|a, b| a.name.cmp(&b.name));
    }
}
```

## Permission Management

### 1. **Tree-Level Permissions**

Implement fine-grained permissions per tree:

```rust
pub struct TreePermissions {
    permissions: HashMap<String, Permission>,  // key_name -> permission level
    default_permission: Option<Permission>,
}

impl TreePermissions {
    /// Check if key has required permission for operation
    pub fn check_permission(&self, key_name: &str, required: Permission) -> Result<()> {
        let key_permission = self.permissions.get(key_name)
            .or(self.default_permission.as_ref())
            .ok_or_else(|| AuthError::UnauthorizedKey {
                key_name: key_name.to_string()
            })?;

        if !key_permission.allows(&required) {
            return Err(AuthError::InsufficientPermission {
                key_name: key_name.to_string(),
                required,
                actual: key_permission.clone(),
            });
        }

        Ok(())
    }

    /// Securely update permissions (Admin only)
    pub fn update_permission(
        &mut self,
        requesting_key: &str,
        target_key: &str,
        new_permission: Permission
    ) -> Result<()> {
        // Verify requesting key has admin permission
        self.check_permission(requesting_key, Permission::Admin)?;

        // Don't allow self-permission changes that would lock out admin
        if requesting_key == target_key && new_permission < Permission::Admin {
            return Err(AuthError::SelfPermissionReduction);
        }

        self.permissions.insert(target_key.to_string(), new_permission);
        Ok(())
    }
}
```

### 2. **Operation-Specific Authorization**

Different operations require different permission levels:

```rust
pub enum OperationType {
    ReadData,
    WriteData,
    WriteSettings,
    ManagePermissions,
}

impl OperationType {
    pub fn required_permission(&self) -> Permission {
        match self {
            Self::ReadData => Permission::Read,
            Self::WriteData => Permission::Write,
            Self::WriteSettings => Permission::Admin,
            Self::ManagePermissions => Permission::Admin,
        }
    }
}

pub fn authorize_operation(
    tree_permissions: &TreePermissions,
    key_name: &str,
    operation: OperationType
) -> Result<()> {
    let required = operation.required_permission();
    tree_permissions.check_permission(key_name, required)
}
```

## Secure Data Handling

### 1. **Input Validation**

Validate all inputs to prevent injection and malformation attacks:

```rust
pub struct DataValidator;

impl DataValidator {
    /// Validate entry ID format
    pub fn validate_entry_id(id: &str) -> Result<()> {
        // Entry IDs should be hex-encoded SHA-256 hashes
        if id.len() != 64 {
            return Err(DataError::InvalidEntryId {
                id: id.to_string(),
                reason: "Invalid length".to_string(),
            });
        }

        if !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(DataError::InvalidEntryId {
                id: id.to_string(),
                reason: "Non-hex characters".to_string(),
            });
        }

        Ok(())
    }

    /// Validate key name format
    pub fn validate_key_name(key_name: &str) -> Result<()> {
        // Key names should be safe identifiers
        if key_name.is_empty() || key_name.len() > 256 {
            return Err(AuthError::InvalidKeyFormat {
                details: "Key name length out of bounds".to_string(),
            });
        }

        // Only allow safe characters
        if !key_name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
            return Err(AuthError::InvalidKeyFormat {
                details: "Key name contains invalid characters".to_string(),
            });
        }

        Ok(())
    }

    /// Validate subtree name
    pub fn validate_subtree_name(name: &str) -> Result<()> {
        // Subtree names should not conflict with reserved names
        const RESERVED_NAMES: &[&str] = &["_settings", "_root"];

        if RESERVED_NAMES.contains(&name) {
            return Err(SubtreeError::ReservedSubtreeName {
                name: name.to_string(),
            });
        }

        // Additional validation rules...
        Ok(())
    }
}
```

### 2. **Secure Serialization**

Prevent deserialization attacks and ensure data integrity:

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct SecureEntry {
    #[serde(deserialize_with = "validate_deserialize_id")]
    pub id: ID,

    #[serde(deserialize_with = "validate_deserialize_data")]
    pub data: RawData,

    pub signature: EntrySignature,
}

/// Custom deserializer with validation
fn validate_deserialize_id<'de, D>(deserializer: D) -> Result<ID, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let id_string = String::deserialize(deserializer)?;
    DataValidator::validate_entry_id(&id_string)
        .map_err(serde::de::Error::custom)?;
    Ok(ID::from(id_string))
}

fn validate_deserialize_data<'de, D>(deserializer: D) -> Result<RawData, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let data = String::deserialize(deserializer)?;

    // Validate data size limits
    if data.len() > MAX_ENTRY_SIZE {
        return Err(serde::de::Error::custom("Entry data too large"));
    }

    // Validate JSON structure if expected
    if let Err(e) = serde_json::from_str::<serde_json::Value>(&data) {
        return Err(serde::de::Error::custom(format!("Invalid JSON: {}", e)));
    }

    Ok(data)
}

const MAX_ENTRY_SIZE: usize = 10 * 1024 * 1024;  // 10MB limit
```

## Attack Prevention

### 1. **Denial of Service Protection**

Implement resource limits and rate limiting:

```rust
pub struct ResourceLimits {
    max_entry_size: usize,
    max_subtrees_per_entry: usize,
    max_parents_per_node: usize,
    max_operations_per_second: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_entry_size: 10 * 1024 * 1024,  // 10MB
            max_subtrees_per_entry: 100,
            max_parents_per_node: 1000,
            max_operations_per_second: 1000,
        }
    }
}

pub struct RateLimiter {
    operations: HashMap<String, VecDeque<Instant>>,  // key_name -> operation times
    limits: ResourceLimits,
}

impl RateLimiter {
    pub fn check_rate_limit(&mut self, key_name: &str) -> Result<()> {
        let now = Instant::now();
        let operations = self.operations.entry(key_name.to_string()).or_default();

        // Remove operations older than 1 second
        while let Some(&front_time) = operations.front() {
            if now.duration_since(front_time).as_secs() >= 1 {
                operations.pop_front();
            } else {
                break;
            }
        }

        // Check if rate limit exceeded
        if operations.len() >= self.limits.max_operations_per_second as usize {
            return Err(AuthError::RateLimitExceeded {
                key_name: key_name.to_string(),
                limit: self.limits.max_operations_per_second,
            });
        }

        operations.push_back(now);
        Ok(())
    }
}
```

### 2. **Hash Collision Protection**

Use secure hash functions and validate hash integrity:

```rust
use sha2::{Sha256, Digest};

pub struct SecureHasher;

impl SecureHasher {
    /// Generate secure content-addressable ID
    pub fn generate_entry_id(canonical_data: &[u8]) -> ID {
        let mut hasher = Sha256::new();
        hasher.update(canonical_data);
        let hash_bytes = hasher.finalize();

        // Convert to hex string
        let hex_string = hex::encode(hash_bytes);
        ID::from(hex_string)
    }

    /// Verify entry ID matches content
    pub fn verify_entry_id(entry: &Entry) -> Result<()> {
        let canonical_bytes = entry.to_canonical_bytes()?;
        let computed_id = Self::generate_entry_id(&canonical_bytes);

        if computed_id != entry.id() {
            return Err(DataError::HashMismatch {
                expected: entry.id().to_string(),
                computed: computed_id.to_string(),
            });
        }

        Ok(())
    }
}
```

### 3. **Timing Attack Prevention**

Use constant-time operations for security-sensitive comparisons:

```rust
use subtle::ConstantTimeEq;

pub fn secure_compare_signatures(sig1: &Signature, sig2: &Signature) -> bool {
    // Use constant-time comparison to prevent timing attacks
    sig1.to_bytes().ct_eq(&sig2.to_bytes()).into()
}

pub fn secure_compare_keys(key1: &str, key2: &str) -> bool {
    // Pad to same length to prevent timing attacks on length
    let max_len = key1.len().max(key2.len());
    let padded1 = format!("{:width$}", key1, width = max_len);
    let padded2 = format!("{:width$}", key2, width = max_len);

    padded1.as_bytes().ct_eq(padded2.as_bytes()).into()
}
```

## Audit and Logging

### 1. **Security Event Logging**

Log security-relevant events for monitoring:

```rust
pub struct SecurityAuditLog {
    events: Vec<SecurityEvent>,
}

#[derive(Debug, Clone)]
pub struct SecurityEvent {
    timestamp: SystemTime,
    event_type: SecurityEventType,
    key_name: Option<String>,
    details: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum SecurityEventType {
    AuthenticationSuccess,
    AuthenticationFailure,
    PermissionDenied,
    RateLimitExceeded,
    InvalidSignature,
    KeyGenerated,
    PermissionChanged,
}

impl SecurityAuditLog {
    pub fn log_event(&mut self, event_type: SecurityEventType, key_name: Option<String>) {
        let event = SecurityEvent {
            timestamp: SystemTime::now(),
            event_type,
            key_name,
            details: HashMap::new(),
        };

        self.events.push(event);

        // Also log to external system if configured
        if let Ok(external_logger) = std::env::var("EIDETICA_AUDIT_ENDPOINT") {
            self.send_to_external_audit(&external_logger, &event);
        }
    }

    fn send_to_external_audit(&self, endpoint: &str, event: &SecurityEvent) {
        // TODO: Implement external audit logging
        // This could send to syslog, external SIEM, etc.
    }
}
```

### 2. **Intrusion Detection**

Monitor for suspicious patterns:

```rust
pub struct IntrusionDetector {
    failed_attempts: HashMap<String, Vec<Instant>>,
    suspicious_patterns: HashMap<String, u32>,
}

impl IntrusionDetector {
    pub fn check_suspicious_activity(&mut self, key_name: &str, operation: &str) -> SecurityAlert {
        // Track failed authentication attempts
        if operation == "authentication_failure" {
            let attempts = self.failed_attempts.entry(key_name.to_string()).or_default();
            attempts.push(Instant::now());

            // Remove old attempts (older than 1 hour)
            let cutoff = Instant::now() - Duration::from_secs(3600);
            attempts.retain(|&time| time > cutoff);

            if attempts.len() > 10 {
                return SecurityAlert::BruteForceAttempt {
                    key_name: key_name.to_string(),
                    attempt_count: attempts.len(),
                };
            }
        }

        // Check for unusual patterns
        let pattern_key = format!("{}:{}", key_name, operation);
        let count = self.suspicious_patterns.entry(pattern_key).or_insert(0);
        *count += 1;

        if *count > 100 {
            return SecurityAlert::UnusualActivity {
                key_name: key_name.to_string(),
                operation: operation.to_string(),
                frequency: *count,
            };
        }

        SecurityAlert::None
    }
}

#[derive(Debug)]
pub enum SecurityAlert {
    None,
    BruteForceAttempt { key_name: String, attempt_count: usize },
    UnusualActivity { key_name: String, operation: String, frequency: u32 },
    RateLimitExceeded { key_name: String },
}
```

## Common Security Anti-Patterns

### ❌ **Insecure Key Storage**

```rust
// DON'T DO THIS - keys in plain text
pub struct InsecureKeyStore {
    private_keys: HashMap<String, String>,  // Plain text private keys
}
```

### ❌ **Missing Input Validation**

```rust
// DON'T DO THIS - no validation
pub fn create_entry(raw_data: String) -> Entry {
    Entry::new(raw_data)  // No validation of input
}
```

### ❌ **Information Leakage in Errors**

```rust
// DON'T DO THIS - leaks sensitive information
#[error("Authentication failed: private key '{private_key}' not found")]
AuthenticationFailed { private_key: String },
```

### ❌ **Weak Random Number Generation**

```rust
// DON'T DO THIS - predictable randomness
use rand::random;
let key_bytes: [u8; 32] = random();  // Not cryptographically secure
```

### ✅ **Secure Patterns**

```rust
// DO THIS - secure key storage with proper zeroing
pub struct SecureKeyStore {
    private_keys: HashMap<String, SecretKey>,  // Proper key type
}

// DO THIS - comprehensive input validation
pub fn create_entry(raw_data: String) -> Result<Entry> {
    DataValidator::validate_raw_data(&raw_data)?;
    Entry::new(raw_data)
}

// DO THIS - generic error messages
#[error("Authentication failed")]
AuthenticationFailed,

// DO THIS - cryptographically secure randomness
use rand::rngs::OsRng;
let mut csprng = OsRng;
let signing_key = SigningKey::generate(&mut csprng);
```

## Future Security Improvements

### Planned Enhancements

- **TODO**: Implement key rotation and versioning system
- **TODO**: Add support for hardware security modules (HSMs)
- **TODO**: Design multi-signature authentication for high-security operations
- **TODO**: Implement zero-knowledge proof integration for privacy
- **TODO**: Add post-quantum cryptographic algorithm support

### Security Monitoring Evolution

- **TODO**: Implement real-time security monitoring dashboard
- **TODO**: Add integration with external SIEM systems
- **TODO**: Design automated threat response capabilities
- **TODO**: Create security metrics and KPI tracking

## Compliance Considerations

### Data Protection

- **TODO**: Evaluate GDPR compliance requirements for personal data
- **TODO**: Implement data retention and deletion policies
- **TODO**: Design audit trail requirements for regulatory compliance
- **TODO**: Plan for data sovereignty and geographic restrictions

### Cryptographic Standards

- **TODO**: Ensure FIPS 140-2 compliance for cryptographic operations
- **TODO**: Plan for Common Criteria evaluation if required
- **TODO**: Monitor NIST recommendations for cryptographic algorithms
- **TODO**: Implement cryptographic algorithm agility for future upgrades

## Summary

Effective security in Eidetica encompasses:

- **Strong authentication** with Ed25519 digital signatures
- **Fine-grained authorization** with hierarchical permissions
- **Secure cryptographic operations** with proper key management
- **Input validation** and data integrity checking
- **Attack prevention** through rate limiting and resource controls
- **Comprehensive auditing** and intrusion detection
- **Future-ready design** for evolving security requirements

Following these patterns ensures the system maintains strong security posture while remaining usable and performant.
