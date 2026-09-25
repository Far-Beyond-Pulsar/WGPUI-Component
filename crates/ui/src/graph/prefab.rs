use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Serialized prefab asset stored in .prefab files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrefabAsset {
    pub prefab_version: u32,
    pub name: String,
    pub components: Vec<ComponentInstance>,
    pub blueprint_class: Option<BlueprintClassRef>,
}

/// Single serialized component instance in a prefab.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComponentInstance {
    /// Stable id of this component slot within the class. Placed instances
    /// key their overrides and generated child objects by it, and
    /// `get_component_ref` nodes resolve through it. Files written before
    /// slot ids existed have none; readers assign `<Class>_<n>` (the n-th
    /// component of that class), as `pulsar_class::PrefabAsset` does.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub slot_id: String,
    pub component_type: String,
    pub properties: HashMap<String, serde_json::Value>,
}

/// Optional blueprint attachment and default overrides.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlueprintClassRef {
    /// Path to blueprint class folder or compiled bytecode.
    pub class_path: String,
    /// Variable default overrides supplied by the prefab.
    #[serde(default)]
    pub variable_defaults: HashMap<String, serde_json::Value>,
}

impl PrefabAsset {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            prefab_version: 1,
            name: name.into(),
            components: Vec::new(),
            blueprint_class: None,
        }
    }
}
