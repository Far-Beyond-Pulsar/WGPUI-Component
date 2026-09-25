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
    /// UUID of this component slot, unique within its class. The class's
    /// compiled script names the component by it and levels key
    /// per-instance overrides by it; placing the class resolves it once into
    /// a handle to the instance's real component. Files without one get a
    /// fresh UUID from their reader (`pulsar_class::PrefabAsset`, the
    /// Blueprint editor), saved back once.
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
