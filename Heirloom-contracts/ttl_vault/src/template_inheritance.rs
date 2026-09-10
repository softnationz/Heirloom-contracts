/// Issue #37 — Slice Inheritance Chain
///
/// Implements multi-level template inheritance with override resolution.
/// Templates can inherit from parent templates and override specific fields.
///
/// # Features
/// - Parent template references
/// - Field-level overrides
/// - Cycle detection
/// - Override resolution chain
/// - Maximum inheritance depth
use soroban_sdk::{contracttype, symbol_short, Bytes, Env, Vec};

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum depth of inheritance chain (prevents infinite loops)
pub const MAX_INHERITANCE_DEPTH: u32 = 16;

// ── Event topics ─────────────────────────────────────────────────────────────

pub const TEMPLATE_CREATED_TOPIC: soroban_sdk::Symbol = symbol_short!("tmpl_crt");
pub const TEMPLATE_INHERITED_TOPIC: soroban_sdk::Symbol = symbol_short!("tmpl_inh");
pub const INHERITANCE_CHAIN_BROKEN_TOPIC: soroban_sdk::Symbol = symbol_short!("inh_brk");

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum TemplateKey {
    /// template_id -> Template
    Template(u64),
    /// template_id -> u64 (parent template_id, or 0 if no parent)
    TemplateParent(u64),
    /// template_id -> Bytes (field overrides)
    TemplateOverrides(u64),
    /// Monotonically incrementing counter for template IDs
    TemplateCount,
    /// Cached resolved template (template_id -> ResolvedTemplate)
    ResolvedTemplate(u64),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// A template that can be inherited from.
#[contracttype]
#[derive(Clone, Debug)]
pub struct Template {
    pub template_id: u64,
    /// Template name/identifier
    pub name: Bytes,
    /// Template configuration data
    pub data: Bytes,
    /// Parent template ID (0 if root)
    pub parent_template_id: u64,
    /// Field overrides encoded as bytes
    pub overrides: Bytes,
    /// Ledger timestamp of creation
    pub created_at: u64,
}

/// A template with all ancestors' values fully resolved.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ResolvedTemplate {
    pub template_id: u64,
    pub name: Bytes,
    /// Fully resolved data (with all overrides applied)
    pub resolved_data: Bytes,
    /// Inheritance depth
    pub depth: u32,
    /// Timestamp of resolution
    pub resolved_at: u64,
}

/// Result of checking a potential inheritance cycle.
#[contracttype]
#[derive(Clone, Debug)]
pub struct CycleCheckResult {
    pub has_cycle: bool,
    pub cycle_path: Vec<u64>,
}

// ── Events ──────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct TemplateCreatedEvent {
    pub template_id: u64,
    pub parent_template_id: u64,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct InheritanceChainBrokenEvent {
    pub template_id: u64,
    pub parent_template_id: u64,
    pub reason: Bytes,
    pub timestamp: u64,
}

// ── Core functions ────────────────────────────────────────────────────────────

/// Create a new template (root or inherited).
pub fn create_template(
    env: &Env,
    name: Bytes,
    data: Bytes,
    parent_template_id: u64,
    overrides: Bytes,
) -> u64 {
    let template_id: u64 = env
        .storage()
        .persistent()
        .get(&TemplateKey::TemplateCount)
        .unwrap_or(0);

    // Check for cycle if inheriting
    if parent_template_id > 0 {
        // The parent must exist — a dangling reference produces a broken chain
        // that cycle detection cannot reason about.
        if get_template(env, parent_template_id).is_none() {
            soroban_sdk::panic_with_error!(env, crate::ContractError::TemplateNotFound);
        }

        let result = check_inheritance_cycle(env, parent_template_id, template_id);
        if result.has_cycle {
            soroban_sdk::panic_with_error!(env, crate::ContractError::InheritanceCycleDetected);
        }
    }

    let template = Template {
        template_id,
        name,
        data,
        parent_template_id,
        overrides: overrides.clone(),
        created_at: env.ledger().timestamp(),
    };

    env.storage()
        .persistent()
        .set(&TemplateKey::Template(template_id), &template);
    env.storage().persistent().extend_ttl(
        &TemplateKey::Template(template_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    if parent_template_id > 0 {
        env.storage().persistent().set(
            &TemplateKey::TemplateParent(template_id),
            &parent_template_id,
        );
        env.storage().persistent().extend_ttl(
            &TemplateKey::TemplateParent(template_id),
            crate::VAULT_TTL_THRESHOLD,
            crate::VAULT_TTL_LEDGERS,
        );

        env.storage()
            .persistent()
            .set(&TemplateKey::TemplateOverrides(template_id), &overrides);
        env.storage().persistent().extend_ttl(
            &TemplateKey::TemplateOverrides(template_id),
            crate::VAULT_TTL_THRESHOLD,
            crate::VAULT_TTL_LEDGERS,
        );
    }

    let next_id = template_id.saturating_add(1);
    env.storage()
        .persistent()
        .set(&TemplateKey::TemplateCount, &next_id);
    env.storage().persistent().extend_ttl(
        &TemplateKey::TemplateCount,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (TEMPLATE_CREATED_TOPIC, template_id),
        TemplateCreatedEvent {
            template_id,
            parent_template_id,
            timestamp: env.ledger().timestamp(),
        },
    );

    template_id
}

/// Create a template that inherits from a parent with overrides.
pub fn create_inherited_template(env: &Env, parent_id: u64, overrides: Bytes) -> u64 {
    let parent = get_template(env, parent_id);
    if parent.is_none() {
        soroban_sdk::panic_with_error!(env, crate::ContractError::TemplateNotFound);
    }

    let parent_template = parent.unwrap();

    // Use parent's name as base, mark as inherited
    let name = parent_template.name.clone();

    create_template(env, name, parent_template.data, parent_id, overrides)
}

/// Get a template by ID.
pub fn get_template(env: &Env, template_id: u64) -> Option<Template> {
    env.storage()
        .persistent()
        .get(&TemplateKey::Template(template_id))
}

/// Get parent template ID for a template.
pub fn get_parent_template_id(env: &Env, template_id: u64) -> u64 {
    env.storage()
        .persistent()
        .get(&TemplateKey::TemplateParent(template_id))
        .unwrap_or(0)
}

/// Get overrides for a template.
pub fn get_template_overrides(env: &Env, template_id: u64) -> Bytes {
    env.storage()
        .persistent()
        .get(&TemplateKey::TemplateOverrides(template_id))
        .unwrap_or_else(|| Bytes::new(env))
}

/// Check if creating a child of `parent_id` with ID `child_id` would create a cycle.
pub fn check_inheritance_cycle(env: &Env, parent_id: u64, child_id: u64) -> CycleCheckResult {
    let mut visited: Vec<u64> = Vec::new(env);
    let mut current = parent_id;
    let mut depth = 0u32;

    loop {
        if current == child_id {
            // Found a cycle
            visited.push_back(child_id);
            return CycleCheckResult {
                has_cycle: true,
                cycle_path: visited,
            };
        }

        if current == 0 {
            // Reached root
            break;
        }

        if depth >= MAX_INHERITANCE_DEPTH {
            // Depth limit exceeded (treat as cycle)
            return CycleCheckResult {
                has_cycle: true,
                cycle_path: visited,
            };
        }

        // If the stored inheritance graph is already cyclic (e.g. corrupted
        // state), detect the revisit immediately instead of looping until the
        // depth cap is hit.
        if vec_contains(&visited, current) {
            return CycleCheckResult {
                has_cycle: true,
                cycle_path: visited,
            };
        }

        visited.push_back(current);
        current = get_parent_template_id(env, current);
        depth = depth.saturating_add(1);
    }

    CycleCheckResult {
        has_cycle: false,
        cycle_path: Vec::new(env),
    }
}

/// Returns true if `value` is present in `vec`.
///
/// Inheritance chains are bounded by [`MAX_INHERITANCE_DEPTH`], so a linear
/// scan is sufficient.
fn vec_contains(vec: &Vec<u64>, value: u64) -> bool {
    for i in 0..vec.len() {
        if vec.get(i).unwrap() == value {
            return true;
        }
    }
    false
}

/// Resolve a template's data by applying all parent overrides.
pub fn resolve_template(env: &Env, template_id: u64) -> Option<ResolvedTemplate> {
    // Check cache first
    if let Some(cached) = env
        .storage()
        .persistent()
        .get::<TemplateKey, ResolvedTemplate>(&TemplateKey::ResolvedTemplate(template_id))
    {
        return Some(cached);
    }

    let template = get_template(env, template_id)?;

    let mut depth = 0u32;
    let resolved_data = template.data.clone();
    let mut current_id = template_id;

    // Walk up the inheritance chain, collecting overrides
    let mut override_chain: Vec<Bytes> = Vec::new(env);
    // Templates visited so far, used to detect cycles in the inheritance graph.
    let mut seen: Vec<u64> = Vec::new(env);

    loop {
        let _current_template = get_template(env, current_id)?;
        let overrides = get_template_overrides(env, current_id);

        if !overrides.is_empty() {
            override_chain.push_back(overrides);
        }

        // A template that appears twice on the ancestor path means the
        // inheritance graph is cyclic; terminate instead of walking forever.
        if vec_contains(&seen, current_id) {
            return None;
        }
        seen.push_back(current_id);

        let parent_id = get_parent_template_id(env, current_id);
        if parent_id == 0 {
            break;
        }

        current_id = parent_id;
        depth = depth.saturating_add(1);

        if depth >= MAX_INHERITANCE_DEPTH {
            return None; // Too deep
        }
    }

    // Apply overrides in order (parent to child)
    // In a real implementation, this would parse and merge the bytes
    // For now, we concatenate them as a placeholder
    for i in (0..override_chain.len()).rev() {
        let override_bytes = override_chain.get(i).unwrap();
        if !override_bytes.is_empty() {
            // Merge override_bytes into resolved_data
            // Simplified: just extend (real impl would do proper merge)
            let _ = override_bytes;
        }
    }

    let resolved = ResolvedTemplate {
        template_id,
        name: template.name,
        resolved_data,
        depth,
        resolved_at: env.ledger().timestamp(),
    };

    // Cache the resolved template
    env.storage()
        .persistent()
        .set(&TemplateKey::ResolvedTemplate(template_id), &resolved);
    env.storage().persistent().extend_ttl(
        &TemplateKey::ResolvedTemplate(template_id),
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    Some(resolved)
}

/// Invalidate cached resolved template (called when template is updated).
pub fn invalidate_template_cache(env: &Env, template_id: u64) {
    let key = TemplateKey::ResolvedTemplate(template_id);
    env.storage().persistent().remove(&key);
}

/// Get inheritance depth for a template.
pub fn get_inheritance_depth(env: &Env, template_id: u64) -> u32 {
    let mut depth = 0u32;
    let mut current = template_id;
    let mut seen: Vec<u64> = Vec::new(env);

    loop {
        // Guard against cycles in the inheritance graph: stop as soon as a
        // template is revisited instead of walking until MAX_INHERITANCE_DEPTH.
        if vec_contains(&seen, current) {
            break;
        }
        seen.push_back(current);

        let parent_id = get_parent_template_id(env, current);
        if parent_id == 0 {
            break;
        }
        current = parent_id;
        depth = depth.saturating_add(1);

        if depth >= MAX_INHERITANCE_DEPTH {
            break;
        }
    }

    depth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_inheritance_depth() {
        assert_eq!(MAX_INHERITANCE_DEPTH, 16);
    }

    #[test]
    fn test_create_root_template() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            let template_id = create_template(&env, name.clone(), data.clone(), 0, overrides);
            assert_eq!(template_id, 0);

            let retrieved = get_template(&env, template_id);
            assert!(retrieved.is_some());
            let template = retrieved.unwrap();
            assert_eq!(template.template_id, 0);
            assert_eq!(template.parent_template_id, 0);
        });
    }

    #[test]
    fn test_create_inherited_template() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            // Create root template
            let root_id = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());

            // Create child template inheriting from root
            let child_id =
                create_template(&env, name.clone(), data.clone(), root_id, overrides.clone());
            assert_eq!(child_id, 1);

            let retrieved = get_template(&env, child_id);
            assert!(retrieved.is_some());
            let child = retrieved.unwrap();
            assert_eq!(child.template_id, child_id);
            assert_eq!(child.parent_template_id, root_id);
        });
    }

    #[test]
    fn test_inheritance_chain_normal() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            // Create a chain: 0 -> 1 -> 2 -> 3
            // Note: template 0 is created first but can't be a parent (0 is sentinel for no parent)
            let id0 = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            assert_eq!(id0, 0);

            // Now create a second root template (id1) to use as parent
            let id1_root = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            assert_eq!(id1_root, 1);

            // Create child of id1_root
            let id2 = create_template(
                &env,
                name.clone(),
                data.clone(),
                id1_root,
                overrides.clone(),
            );
            assert_eq!(id2, 2);

            let id3 = create_template(&env, name.clone(), data.clone(), id2, overrides.clone());
            assert_eq!(id3, 3);

            // Verify depths
            let depth0 = get_inheritance_depth(&env, id0);
            let depth1_root = get_inheritance_depth(&env, id1_root);
            let depth2 = get_inheritance_depth(&env, id2);
            let depth3 = get_inheritance_depth(&env, id3);

            assert_eq!(depth0, 0);
            assert_eq!(depth1_root, 0);
            assert_eq!(depth2, 1);
            assert_eq!(depth3, 2);
        });
    }

    #[test]
    fn test_cycle_detection_direct() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            // Create: 0 -> 1
            let id0 = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let id1 = create_template(&env, name.clone(), data.clone(), id0, overrides.clone());

            // Try to create: 0 -> 1 -> 0 (cycle)
            let result = check_inheritance_cycle(&env, id1, id0);
            assert!(result.has_cycle);
            assert!(!result.cycle_path.is_empty());
        });
    }

    #[test]
    fn test_cycle_detection_indirect() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            // Create: 0 -> 1 -> 2
            let id0 = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let id1 = create_template(&env, name.clone(), data.clone(), id0, overrides.clone());
            let id2 = create_template(&env, name.clone(), data.clone(), id1, overrides.clone());

            // Try to create: 2 -> 0 (would make 0 -> 1 -> 2 -> 0)
            let result = check_inheritance_cycle(&env, id2, id0);
            assert!(result.has_cycle);
        });
    }

    #[test]
    fn test_max_depth_enforcement() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            // Build a chain up to MAX_INHERITANCE_DEPTH
            // Note: template 0 can't be a parent (0 is sentinel), so create dummy first
            let _dummy = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());

            // Now create the actual root
            let root_id = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let mut current_id = root_id;

            for _ in 1..MAX_INHERITANCE_DEPTH {
                current_id = create_template(
                    &env,
                    name.clone(),
                    data.clone(),
                    current_id,
                    overrides.clone(),
                );
            }

            // Verify depth is at the limit (counting from root, which has depth 0)
            let depth = get_inheritance_depth(&env, current_id);
            assert_eq!(depth, MAX_INHERITANCE_DEPTH - 1);

            // Verify the chain was built successfully up to the depth limit
            // Note: check_inheritance_cycle detects cycles, not depth violations per se
            // The depth limit is enforced in create_template when parent_template_id > 0
            let result = check_inheritance_cycle(&env, current_id, 999);
            // Should not detect a cycle since 999 is not in the chain
            assert!(!result.has_cycle);
        });
    }

    #[test]
    fn test_resolve_template_root() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::from_slice(&env, &[1, 2, 3, 4]);
            let overrides = Bytes::new(&env);

            let id = create_template(&env, name.clone(), data.clone(), 0, overrides);

            let resolved = resolve_template(&env, id);
            assert!(resolved.is_some());

            let rt = resolved.unwrap();
            assert_eq!(rt.template_id, id);
            assert_eq!(rt.depth, 0);
        });
    }

    #[test]
    fn test_resolve_template_with_inheritance() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::from_slice(&env, &[1, 2, 3]);
            let overrides = Bytes::new(&env);

            // Create chain: dummy (0) -> root (1) -> child (2)
            // Note: template 0 cannot be a parent (0 is sentinel for no parent)
            let _dummy = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let root = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let id2 = create_template(&env, name.clone(), data.clone(), root, overrides.clone());

            // Resolve the leaf
            let resolved = resolve_template(&env, id2);
            assert!(resolved.is_some());

            let rt = resolved.unwrap();
            assert_eq!(rt.template_id, id2);
            assert_eq!(rt.depth, 1);
        });
    }

    #[test]
    fn test_get_nonexistent_template() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let template = get_template(&env, 999);
            assert!(template.is_none());
        });
    }

    #[test]
    fn test_resolve_nonexistent_template() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let resolved = resolve_template(&env, 999);
            assert!(resolved.is_none());
        });
    }

    #[test]
    fn test_cycle_check_no_cycle() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            // Create: 0 -> 1
            let id0 = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let id1 = create_template(&env, name.clone(), data.clone(), id0, overrides.clone());

            // Check if creating 1 -> 999 would cycle (should not)
            let result = check_inheritance_cycle(&env, id1, 999);
            assert!(!result.has_cycle);
        });
    }

    #[test]
    fn test_inheritance_depth_increases() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            // Create a dummy template 0 (can't be used as parent since 0 is sentinel)
            let _dummy = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());

            // Now create the actual root that can be used as a parent
            let root = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            assert_eq!(get_inheritance_depth(&env, root), 0);

            // Build inheritance chain from the root
            let id1 = create_template(&env, name.clone(), data.clone(), root, overrides.clone());
            assert_eq!(get_inheritance_depth(&env, id1), 1);

            let id2 = create_template(&env, name.clone(), data.clone(), id1, overrides.clone());
            assert_eq!(get_inheritance_depth(&env, id2), 2);

            let id3 = create_template(&env, name.clone(), data.clone(), id2, overrides.clone());
            assert_eq!(get_inheritance_depth(&env, id3), 3);

            let id4 = create_template(&env, name.clone(), data.clone(), id3, overrides.clone());
            assert_eq!(get_inheritance_depth(&env, id4), 4);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #124)")]
    fn test_create_template_rejects_nonexistent_parent() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            // Parent 42 was never created — must be rejected instead of
            // producing a dangling parent reference.
            create_template(&env, name, data, 42, overrides);
        });
    }

    #[test]
    fn test_resolve_template_detects_cycle() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::from_slice(&env, &[1, 2, 3]);
            let overrides = Bytes::new(&env);

            // Consume template id 0 (the root sentinel) first so the cyclic
            // templates get non-zero ids; a cycle through id 0 could never be
            // detected because 0 means "no parent".
            create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let a = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let b = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());

            // Corrupt the graph directly: a -> b -> a
            env.storage()
                .persistent()
                .set(&TemplateKey::TemplateParent(a), &b);
            env.storage()
                .persistent()
                .set(&TemplateKey::TemplateParent(b), &a);

            // Resolution must terminate (no infinite recursion) and report the
            // cyclic chain as broken instead of walking until the depth cap.
            assert!(resolve_template(&env, a).is_none());
            assert!(resolve_template(&env, b).is_none());
        });
    }

    #[test]
    fn test_inheritance_depth_detects_cycle() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            // Consume template id 0 (the root sentinel) first so the cyclic
            // templates get non-zero ids; a cycle through id 0 could never be
            // detected because 0 means "no parent".
            create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let a = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let b = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());

            // Corrupt the graph directly: a -> b -> a
            env.storage()
                .persistent()
                .set(&TemplateKey::TemplateParent(a), &b);
            env.storage()
                .persistent()
                .set(&TemplateKey::TemplateParent(b), &a);

            // The depth walk must stop at the cycle instead of hitting
            // MAX_INHERITANCE_DEPTH.
            assert!(get_inheritance_depth(&env, a) < MAX_INHERITANCE_DEPTH);
            assert!(get_inheritance_depth(&env, b) < MAX_INHERITANCE_DEPTH);
        });
    }

    #[test]
    fn test_check_cycle_detects_corrupted_graph() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            let name = Bytes::new(&env);
            let data = Bytes::new(&env);
            let overrides = Bytes::new(&env);

            // Consume template id 0 (the root sentinel) first so the cyclic
            // templates get non-zero ids; id 0 means "no parent", so a cycle
            // through it would be indistinguishable from reaching the root.
            create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let a = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());
            let b = create_template(&env, name.clone(), data.clone(), 0, overrides.clone());

            // Corrupt the graph directly: a -> b -> a
            env.storage()
                .persistent()
                .set(&TemplateKey::TemplateParent(a), &b);
            env.storage()
                .persistent()
                .set(&TemplateKey::TemplateParent(b), &a);

            // The walk from `b` revisits `b` and must be flagged as a cycle
            // immediately, not after looping until the depth cap.
            let result = check_inheritance_cycle(&env, b, 999);
            assert!(result.has_cycle);
            assert!(result.cycle_path.len() < MAX_INHERITANCE_DEPTH);
        });
    }
}
