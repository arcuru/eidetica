# Design Documents

This section contains formal design documents that capture the architectural thinking, decision-making process, and implementation details for complex features in Eidetica. These documents serve as a historical record of our technical decisions and provide context for future development.

## Purpose

Design documents in this section:

- Document the rationale behind major technical decisions
- Capture alternative approaches that were considered
- Outline implementation strategies and tradeoffs
- Serve as a reference for future developers
- Help maintain consistency in architectural decisions

## Document Structure

Each design document typically includes:

- Problem statement and context
- Goals and non-goals
- Proposed solution
- Alternative approaches considered
- Implementation details and tradeoffs
- Future considerations and potential improvements

## Available Design Documents

### Implemented

- [Authentication](authentication.md) - Mandatory cryptographic authentication for all entries
- [Settings Storage](settings_storage.md) - How settings are stored and tracked in databases
- [Subtree Index](subtree_index.md) - Registry system for subtree metadata and type discovery
- [Height Strategy](height_strategy.md) - Configurable height calculation for entry ordering
- [Database Ticket](database_ticket.md) - URL-based shareable database links with transport hints

### Proposed

- [Users](users.md) - Multi-user system with password-based authentication, user isolation, and per-user key management
- [Key Management](key_management.md) - Technical details for key encryption, storage, and discovery in the Users system
- [Error Handling](error_handling.md) - Modular error architecture for improved debugging and user experience
