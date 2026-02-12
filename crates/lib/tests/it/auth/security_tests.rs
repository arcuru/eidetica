use eidetica::auth::{
    crypto::{PrivateKey, format_public_key, generate_keypair},
    settings::AuthSettings,
    types::{AuthKey, KeyStatus, Permission, ResolvedAuth},
};

use super::helpers::*;

// ===== SECURITY ENFORCEMENT TESTS =====
// These tests verify that security measures are properly enforced in the auth system

#[test]
fn test_admin_hierarchy_enforcement() {
    let mut settings = AuthSettings::new();

    let (_, high_pubkey) = generate_keypair();
    let (_, low_pubkey) = generate_keypair();
    let high_pubkey_str = format_public_key(&high_pubkey);
    let low_pubkey_str = format_public_key(&low_pubkey);

    let high_admin = auth_key(Permission::Admin(1), KeyStatus::Active);
    let low_admin = auth_key(Permission::Admin(100), KeyStatus::Active);

    settings.add_key(&high_pubkey_str, high_admin).unwrap();
    settings
        .add_key(&low_pubkey_str, low_admin.clone())
        .unwrap();

    let low_priority_resolved = ResolvedAuth {
        public_key: PrivateKey::generate().public_key(),
        effective_permission: low_admin.permissions().clone(),
        key_status: low_admin.status().clone(),
    };

    let can_modify = settings
        .can_modify_key(&low_priority_resolved, &high_pubkey_str)
        .unwrap();

    // Low priority admin should NOT be able to modify high priority admin
    assert!(
        !can_modify,
        "Low priority admin (priority 100) should not be able to modify high priority admin (priority 1)"
    );
}

#[test]
fn test_permission_ordering_correctness() {
    // Admin permissions should always be higher than Write permissions
    let admin_lowest = Permission::Admin(u32::MAX);
    let write_highest = Permission::Write(0);

    // This should now work correctly after fixing the arithmetic
    assert!(
        admin_lowest > write_highest,
        "Admin should always be higher than Write"
    );
    assert_ne!(
        admin_lowest, write_highest,
        "Admin and Write should never be equal"
    );
}

#[test]
fn test_admin_hierarchy_complete_enforcement() {
    let mut settings = AuthSettings::new();

    // Create a super high-priority admin (priority 0 = absolute highest)
    let (_, super_admin_key) = generate_keypair();
    let super_admin_pubkey = format_public_key(&super_admin_key);
    let super_admin = AuthKey::active(
        Some("super_admin"),
        Permission::Admin(0), // Absolute highest priority
    );

    // Create a very low-priority admin (almost lowest possible)
    let (_, junior_admin_key) = generate_keypair();
    let junior_admin_pubkey = format_public_key(&junior_admin_key);
    let junior_admin = AuthKey::active(
        Some("junior_admin"),
        Permission::Admin(u32::MAX - 1), // Almost lowest priority
    );

    settings
        .add_key(&super_admin_pubkey, super_admin.clone())
        .unwrap();
    settings
        .add_key(&junior_admin_pubkey, junior_admin.clone())
        .unwrap();

    let junior_resolved = ResolvedAuth {
        public_key: PrivateKey::generate().public_key(),
        effective_permission: junior_admin.permissions().clone(),
        key_status: junior_admin.status().clone(),
    };

    // This should NEVER be true - a low priority admin should not be able to modify a super admin
    let can_modify = settings
        .can_modify_key(&junior_resolved, &super_admin_pubkey)
        .unwrap();

    // Junior admin should NEVER be able to modify super admin
    assert!(
        !can_modify,
        "Junior admin (priority {}) should not be able to modify super admin (priority 0)",
        u32::MAX - 1
    );
}

#[test]
fn test_permission_arithmetic_correctness() {
    // Test that permission arithmetic works correctly
    let admin_max_priority = Permission::Admin(0); // Highest priority
    let admin_min_priority = Permission::Admin(u32::MAX); // Lowest priority
    let write_max_priority = Permission::Write(0); // Highest write priority

    // Admin permissions should never equal Write permissions
    assert_ne!(
        admin_min_priority, write_max_priority,
        "Admin and Write should never be equal"
    );

    // Admin should always be higher than Write
    assert!(
        admin_max_priority > write_max_priority,
        "Admin should always be higher than Write"
    );
    assert!(
        admin_min_priority > write_max_priority,
        "Even lowest priority Admin should be higher than highest priority Write"
    );
}

#[test]
fn test_privilege_escalation_prevention() {
    let mut settings = AuthSettings::new();

    // Scenario: A write user can somehow escalate to admin privileges
    // This tests a hypothetical privilege escalation vulnerability

    let (_, write_pubkey) = generate_keypair();
    let (_, admin_pubkey) = generate_keypair();
    let write_pubkey_str = format_public_key(&write_pubkey);
    let admin_pubkey_str = format_public_key(&admin_pubkey);

    let write_user = auth_key(Permission::Write(10), KeyStatus::Active);
    let admin_user = auth_key(Permission::Admin(5), KeyStatus::Active);

    settings
        .add_key(&write_pubkey_str, write_user.clone())
        .unwrap();
    settings
        .add_key(&admin_pubkey_str, admin_user.clone())
        .unwrap();

    let write_resolved = ResolvedAuth {
        public_key: PrivateKey::generate().public_key(),
        effective_permission: write_user.permissions().clone(),
        key_status: write_user.status().clone(),
    };

    // Write users should NEVER be able to modify admin keys
    let can_modify_admin = settings
        .can_modify_key(&write_resolved, &admin_pubkey_str)
        .unwrap();

    // Write users should NEVER be able to create new admin keys
    let can_create_admin = settings
        .can_create_key(&write_resolved, &Permission::Admin(1))
        .unwrap();

    assert!(
        !can_modify_admin && !can_create_admin,
        "Write users should not be able to modify admin keys"
    );
}

#[test]
fn test_key_creation_privilege_escalation_prevention() {
    let mut settings = AuthSettings::new();

    // Create a low-priority admin that should not be able to create high-priority admins
    let (_, low_admin_pubkey) = generate_keypair();
    let low_admin_pubkey_str = format_public_key(&low_admin_pubkey);
    let low_admin = auth_key(
        Permission::Admin(100), // Low priority admin
        KeyStatus::Active,
    );

    settings
        .add_key(&low_admin_pubkey_str, low_admin.clone())
        .unwrap();

    let low_admin_resolved = ResolvedAuth {
        public_key: PrivateKey::generate().public_key(),
        effective_permission: low_admin.permissions().clone(),
        key_status: low_admin.status().clone(),
    };

    // Test that low admin cannot create a higher priority admin key
    let can_create_super_admin = settings
        .can_create_key(&low_admin_resolved, &Permission::Admin(0))
        .unwrap();

    // Low priority admin (priority 100) should NOT be able to create super admin (priority 0)
    assert!(
        !can_create_super_admin,
        "Low priority admin (priority 100) should not be able to create super admin (priority 0)"
    );

    // Test that low admin CAN create lower priority keys
    let can_create_lower_admin = settings
        .can_create_key(&low_admin_resolved, &Permission::Admin(200))
        .unwrap();

    assert!(
        can_create_lower_admin,
        "Low priority admin (priority 100) should be able to create lower priority admin (priority 200)"
    );
}
