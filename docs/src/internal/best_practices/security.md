# Security Best Practices

This document outlines security patterns and practices used throughout the Eidetica codebase.

## Core Security Architecture

### 1. **Authentication System**

Eidetica uses Ed25519 digital signatures for all entry authentication. The system provides high-performance cryptographic verification through content-addressable entries that enable automatic tampering detection. All entries must be signed by authorized keys, with private keys stored separately from synchronized data.

### 2. **Authorization Model**

The system implements a hierarchical permission model with three levels: Read (view data and compute states), Write (create and modify entries), and Admin (manage permissions and authentication settings). Permissions follow a hierarchical structure where higher levels include all lower-level permissions.

### 3. **Secure Entry Creation**

All entries require authentication during creation. The system verifies authentication keys exist and have appropriate permissions before creating entries. Each entry is signed and stored with verification to ensure integrity.

## Cryptographic Best Practices

### 1. **Digital Signature Handling**

Ed25519 signatures provide authentication for all entries. The system creates signatures from canonical byte representations and verifies them against stored public keys to ensure data integrity and authenticity.

### 2. **Key Generation and Storage**

Keys are generated using cryptographically secure random number generators. Private keys are stored separately from public keys and are securely cleared from memory when removed to prevent key material leakage.

### 3. **Canonical Serialization**

The system ensures consistent serialization for signature verification by sorting all fields deterministically and creating canonical JSON representations. This prevents signature verification failures due to serialization differences.

## Permission Management

### 1. **Database-Level Permissions**

Each database maintains fine-grained permissions mapping keys to permission levels. The system checks permissions by looking up key-specific permissions or falling back to default permissions. Admin-only operations include permission updates, with safeguards to prevent self-lockout.

### 2. **Operation-Specific Authorization**

Different operations require different permission levels: reading data requires Read permission, writing data requires Write permission, and managing settings or permissions requires Admin permission. The system enforces these requirements before allowing any operation to proceed.

## Secure Data Handling

### 1. **Input Validation**

All inputs undergo validation to prevent injection and malformation attacks. Entry IDs must be valid hex-encoded SHA-256 hashes, key names must contain only safe alphanumeric characters, and store names cannot conflict with reserved system names. The system enforces strict size limits and character restrictions.

### 2. **Secure Serialization**

The system prevents deserialization attacks through custom deserializers that validate data during parsing. Entry data is subject to size limits and format validation, ensuring only well-formed data enters the system.

## Attack Prevention

### 1. **Denial of Service Protection**

The system implements comprehensive resource limits including maximum entry sizes, store counts, and parent node limits. Rate limiting prevents excessive operations per second from any single key, with configurable thresholds to balance security and usability.

### 2. **Hash Collision Protection**

SHA-256 hashing ensures content-addressable IDs are collision-resistant. The system verifies that entry IDs match their content hash, detecting any tampering or corruption attempts.

### 3. **Timing Attack Prevention**

Security-sensitive comparisons use constant-time operations to prevent timing-based information leakage. This includes signature comparisons and key matching operations.

## Audit and Logging

### 1. **Security Event Logging**

The system logs all security-relevant events including authentication attempts, permission denials, rate limit violations, and key management operations. Events are timestamped and can be forwarded to external monitoring systems for centralized security analysis.

### 2. **Intrusion Detection**

Active monitoring detects suspicious patterns such as repeated authentication failures indicating brute force attempts or unusual operation frequencies suggesting system abuse. The detector maintains sliding time windows to track patterns and generate alerts when thresholds are exceeded.

## Common Security Anti-Patterns

Key security mistakes to avoid include storing private keys in plain text, missing input validation, leaking sensitive information in error messages, and using weak random number generation. Always use proper key types with secure memory handling, validate all inputs, provide generic error messages, and use cryptographically secure random number generators.

## Summary

Effective security in Eidetica encompasses strong authentication with Ed25519 digital signatures, fine-grained authorization with hierarchical permissions, secure cryptographic operations with proper key management, comprehensive input validation, attack prevention through rate limiting and resource controls, and thorough auditing with intrusion detection capabilities.
