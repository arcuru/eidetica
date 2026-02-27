//! Edge case tests for permission clamping and arithmetic
//!
//! These tests cover boundary conditions, overflow scenarios, and complex
//! permission clamping edge cases in the flat delegation structure.

use eidetica::{
    Result,
    auth::{
        permission::{clamp_permission, validate_permission_bounds},
        types::{Permission, PermissionBounds},
    },
};

/// Test permission clamping with invalid bounds (min > max)
#[test]
#[cfg_attr(miri, ignore)] // triggers tracing::warn! which uses SystemTime::now()
fn test_permission_clamping_invalid_bounds() {
    let bounds = PermissionBounds {
        min: Some(Permission::Admin(5)), // Higher privilege
        max: Permission::Write(10),      // Lower privilege
    };

    let key_permission = Permission::Write(7);

    // Should handle invalid bounds gracefully
    let result = clamp_permission(key_permission, &bounds);

    // Since key permission (Write(7)) > max (Write(10)), it gets clamped to max
    assert_eq!(result, Permission::Write(10));
}

/// Test permission clamping with u32::MAX priority values
#[test]
fn test_permission_clamping_max_priority() {
    let bounds = PermissionBounds {
        min: Some(Permission::Write(u32::MAX)),
        max: Permission::Admin(u32::MAX),
    };

    let key_permission = Permission::Admin(0); // Highest priority

    let result = clamp_permission(key_permission, &bounds);

    // Should clamp to max
    assert_eq!(result, Permission::Admin(u32::MAX));
}

/// Test permission clamping with priority overflow scenarios
#[test]
fn test_permission_arithmetic_overflow() {
    // Test edge case where priorities are at extreme values
    let high_priority_admin = Permission::Admin(0);
    let low_priority_write = Permission::Write(u32::MAX);

    // Admin should always be greater than Write regardless of priority
    assert!(high_priority_admin > low_priority_write);

    let max_priority_admin = Permission::Admin(u32::MAX);
    let min_priority_write = Permission::Write(0);

    // Even Admin with max priority should be greater than Write with min priority
    assert!(max_priority_admin > min_priority_write);
}

/// Test permission clamping with None min bound edge cases
#[test]
fn test_permission_clamping_none_min_bound() {
    let bounds = PermissionBounds {
        min: None, // No minimum bound
        max: Permission::Write(10),
    };

    let test_cases = vec![
        (Permission::Admin(0), Permission::Write(10)), // Should clamp down
        (Permission::Write(5), Permission::Write(10)), // Write(5) > Write(10), clamp to max
        (Permission::Write(15), Permission::Write(15)), // Write(15) < Write(10), stays same
        (Permission::Read, Permission::Read),          // Should stay same
    ];

    for (input, expected) in test_cases {
        let result = clamp_permission(input, &bounds);
        assert_eq!(result, expected, "Failed for input: {input:?}");
    }
}

/// Test permission clamping with Read permissions
#[test]
fn test_permission_clamping_read_permissions() {
    let bounds = PermissionBounds {
        min: Some(Permission::Read),
        max: Permission::Write(20),
    };

    let test_cases = vec![
        (Permission::Admin(0), Permission::Write(20)), // Clamp to max
        (Permission::Write(25), Permission::Write(25)), // Write(25) < Write(20) but > Read, stays same
        (Permission::Write(15), Permission::Write(20)), // Write(15) > max Write(20), clamp to max
        (Permission::Read, Permission::Read),           // No change (meets min)
    ];

    for (input, expected) in test_cases {
        let result = clamp_permission(input, &bounds);
        assert_eq!(result, expected, "Failed for input: {input:?}");
    }
}

/// Test permission priority preservation during clamping
#[test]
fn test_permission_priority_preservation() {
    // Test that when permission level stays the same, priority is preserved
    let bounds = PermissionBounds {
        min: Some(Permission::Write(30)), // Lower priority bound
        max: Permission::Admin(5),        // Higher priority bound
    };

    // Permission that falls within bounds should preserve priority
    let within_bounds = Permission::Write(10); // Within bounds (Write(10) < Admin(5) and > Write(30))
    let result = clamp_permission(within_bounds, &bounds);
    assert_eq!(result, Permission::Write(10)); // Should keep original

    // Permission below min should take min's priority
    let below_min = Permission::Write(35); // Lower priority than min
    let result = clamp_permission(below_min, &bounds);
    assert_eq!(result, Permission::Write(30)); // Should use min's priority
}

/// Test permission bounds validation edge cases
#[test]
fn test_permission_bounds_edge_cases() {
    // Test with same permission level for min and max
    // Write(20) < Write(10) in permission ordering, so min < max is valid
    let same_level_bounds = PermissionBounds {
        min: Some(Permission::Write(20)), // Lower permission bound
        max: Permission::Write(10),       // Higher permission bound
    };

    let test_cases = vec![
        (Permission::Write(5), Permission::Write(10)), // Write(5) > max Write(10), clamp to max
        (Permission::Write(15), Permission::Write(15)), // Within range
        (Permission::Write(25), Permission::Write(20)), // Write(25) < min Write(20), clamp to min
        (Permission::Admin(0), Permission::Write(10)), // Different level, clamp to max
        (Permission::Read, Permission::Write(20)),     // Different level, clamp to min
    ];

    for (input, expected) in test_cases {
        let result = clamp_permission(input, &same_level_bounds);
        assert_eq!(result, expected, "Failed for input: {input:?}");
    }
}

/// Test multi-level clamping simulation
#[test]
#[cfg_attr(miri, ignore)] // triggers tracing::warn! which uses SystemTime::now()
fn test_multi_level_clamping_simulation() {
    // Simulate multi-level delegation where each level applies clamping
    let original_permission = Permission::Admin(0); // Highest privilege

    // Level 1: Main tree bounds
    let level1_bounds = PermissionBounds {
        min: None,
        max: Permission::Write(5),
    };
    let level1_result = clamp_permission(original_permission, &level1_bounds);
    assert_eq!(level1_result, Permission::Write(5));

    // Level 2: Another delegation level with tighter bounds
    let level2_bounds = PermissionBounds {
        min: Some(Permission::Write(10)), // Higher number = lower priority
        max: Permission::Write(15),
    };
    let level2_result = clamp_permission(level1_result, &level2_bounds);
    // Write(5) is > Write(15) (max), so it gets clamped to max
    assert_eq!(level2_result, Permission::Write(15));

    // Level 3: Final level
    let level3_bounds = PermissionBounds {
        min: None,
        max: Permission::Read, // Most restrictive
    };
    let final_result = clamp_permission(level2_result, &level3_bounds);
    assert_eq!(final_result, Permission::Read);
}

/// Test permission clamping with extreme priority differences
#[test]
fn test_permission_extreme_priority_differences() {
    let bounds = PermissionBounds {
        min: Some(Permission::Write(u32::MAX)), // Lowest possible priority
        max: Permission::Admin(0),              // Highest possible priority
    };

    let test_cases = vec![
        // Admin(1) < Admin(0) but > Write(u32::MAX), stays same
        (Permission::Admin(1), Permission::Admin(1)),
        // Write(0) < Admin(0) but > Write(u32::MAX), stays same
        (Permission::Write(0), Permission::Write(0)),
        // Write(100) < Admin(0) but > Write(u32::MAX), stays same
        (Permission::Write(100), Permission::Write(100)),
        // Read < Write(u32::MAX), gets raised to min
        (Permission::Read, Permission::Write(u32::MAX)),
    ];

    for (input, expected) in test_cases {
        let result = clamp_permission(input, &bounds);
        assert_eq!(result, expected, "Failed for input: {input:?}");
    }
}

/// Test permission comparison edge cases
#[test]
fn test_permission_comparison_edge_cases() {
    // Test that Admin always > Write regardless of priorities
    assert!(Permission::Admin(u32::MAX) > Permission::Write(0));
    assert!(Permission::Admin(1000) > Permission::Write(0));

    // Test that Write always > Read regardless of priorities
    assert!(Permission::Write(u32::MAX) > Permission::Read);
    assert!(Permission::Write(1000) > Permission::Read);

    // Test that Admin always > Read
    assert!(Permission::Admin(u32::MAX) > Permission::Read);
    assert!(Permission::Admin(0) > Permission::Read);

    // Test priority comparison within same level
    assert!(Permission::Admin(0) > Permission::Admin(1));
    assert!(Permission::Admin(5) > Permission::Admin(10));
    assert!(Permission::Write(0) > Permission::Write(1));
    assert!(Permission::Write(100) > Permission::Write(1000));
}

/// Test permission serialization with extreme values
#[test]
fn test_permission_serialization_extreme_values() -> Result<()> {
    let extreme_permissions = vec![
        Permission::Admin(0),
        Permission::Admin(u32::MAX),
        Permission::Write(0),
        Permission::Write(u32::MAX),
        Permission::Read,
    ];

    for permission in extreme_permissions {
        let serialized = serde_json::to_string(&permission)?;
        let deserialized: Permission = serde_json::from_str(&serialized)?;
        assert_eq!(permission, deserialized);
    }

    Ok(())
}

/// Test permission bounds validation function
#[test]
fn test_permission_bounds_validation() {
    // Valid bounds: min <= max
    let valid_bounds1 = PermissionBounds {
        min: Some(Permission::Write(20)),
        max: Permission::Write(10), // Write(20) < Write(10), so valid
    };
    assert!(validate_permission_bounds(&valid_bounds1));

    let valid_bounds2 = PermissionBounds {
        min: Some(Permission::Read),
        max: Permission::Admin(5), // Read < Admin(5), so valid
    };
    assert!(validate_permission_bounds(&valid_bounds2));

    let valid_bounds3 = PermissionBounds {
        min: None, // No min is always valid
        max: Permission::Read,
    };
    assert!(validate_permission_bounds(&valid_bounds3));

    // Invalid bounds: min > max
    let invalid_bounds1 = PermissionBounds {
        min: Some(Permission::Admin(5)),
        max: Permission::Write(10), // Admin(5) > Write(10), so invalid
    };
    assert!(!validate_permission_bounds(&invalid_bounds1));

    let invalid_bounds2 = PermissionBounds {
        min: Some(Permission::Write(5)),
        max: Permission::Write(10), // Write(5) > Write(10), so invalid
    };
    assert!(!validate_permission_bounds(&invalid_bounds2));
}

/// Test permission bounds with invalid constraints
#[test]
#[cfg_attr(miri, ignore)] // triggers tracing::warn! which uses SystemTime::now()
fn test_permission_bounds_invalid_constraints() {
    // Test valid bounds
    let bounds1 = PermissionBounds {
        min: Some(Permission::Write(20)), // min (lower permission)
        max: Permission::Admin(10),       // max (higher permission)
    };

    // Test invalid bounds (min > max) - this should trigger fallback behavior
    let bounds2 = PermissionBounds {
        min: Some(Permission::Admin(25)), // min > max (invalid)
        max: Permission::Write(5),
    };

    let permission = Permission::Write(15); // Between Write(20) and Admin(10)

    // Apply first bounds (valid)
    let result1 = clamp_permission(permission, &bounds1);
    assert_eq!(result1, Permission::Write(15)); // Within bounds, should stay same

    // Apply second bounds (invalid) - should apply only max bound as fallback
    let result2 = clamp_permission(result1, &bounds2);

    // With invalid bounds, only max bound is applied: Write(15) < Write(5), so stays Write(15)
    assert_eq!(result2, Permission::Write(15));

    // Test with a permission that DOES exceed the max bound in invalid bounds
    let high_permission = Permission::Admin(0); // Highest permission
    let result3 = clamp_permission(high_permission, &bounds2);

    // Admin(0) > Write(5), so it should get clamped to Write(5) even with invalid bounds
    assert_eq!(result3, Permission::Write(5));
}

/// Test permission arithmetic for can_* methods
#[test]
fn test_permission_capabilities_edge_cases() {
    let edge_cases = vec![
        (Permission::Admin(u32::MAX), true, true), // Admin can do everything
        (Permission::Write(u32::MAX), false, true), // Write can't admin
        (Permission::Read, false, false),          // Read only reads
    ];

    for (permission, can_admin, can_write) in edge_cases {
        assert_eq!(
            permission.can_admin(),
            can_admin,
            "can_admin failed for {permission:?}"
        );
        assert_eq!(
            permission.can_write(),
            can_write,
            "can_write failed for {permission:?}"
        );
    }
}

/// Test permission priority extraction edge cases
#[test]
fn test_permission_priority_extraction() {
    let cases = vec![
        (Permission::Admin(0), Some(0)),
        (Permission::Admin(u32::MAX), Some(u32::MAX)),
        (Permission::Write(42), Some(42)),
        (Permission::Write(u32::MAX), Some(u32::MAX)),
        (Permission::Read, None), // Read has no priority
    ];

    for (permission, expected_priority) in cases {
        assert_eq!(
            permission.priority(),
            expected_priority,
            "Priority failed for {permission:?}"
        );
    }
}
