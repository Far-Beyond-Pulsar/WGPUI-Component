//! Blueprint graph data model.
//!
//! Contains the full `BlueprintAsset`, `GraphDescription`, node/pin/connection
//! types, and the sub-graph / macro system.  All types are pure data — they
//! have no dependency on any engine crate.

pub mod prefab;
pub mod type_system;

pub use prefab::{BlueprintClassRef, ComponentInstance as PrefabComponentInstance, PrefabAsset};
pub use type_system::*;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

/// Unified blueprint file format containing everything (like Unreal Engine).
/// This is the top-level structure that gets saved to .bp_graph files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintAsset {
    /// File format version for backward compatibility
    pub format_version: u32,

    /// The main event graph
    pub main_graph: GraphDescription,

    /// All local macro graphs defined in this blueprint
    #[serde(default)]
    pub local_macros: Vec<SubGraphDefinition>,

    /// Class variables
    #[serde(default)]
    pub variables: Vec<ClassVariable>,

    /// Editor-only data (open tabs, UI state, etc.)
    #[serde(default)]
    pub editor_state: Option<BlueprintEditorState>,

    /// Blueprint metadata (type, parent class, etc.)
    #[serde(default)]
    pub blueprint_metadata: BlueprintMetadata,
}

/// Blueprint metadata for context sensitivity and organisation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintMetadata {
    #[serde(default)]
    pub blueprint_type: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_class: Option<String>,

    #[serde(default)]
    pub description: String,

    #[serde(default)]
    pub category: String,

    #[serde(default)]
    pub tags: Vec<String>,
}

impl Default for BlueprintMetadata {
    fn default() -> Self {
        Self {
            blueprint_type: "Generic".to_string(),
            parent_class: None,
            description: String::new(),
            category: "Uncategorized".to_string(),
            tags: Vec::new(),
        }
    }
}

/// Editor state for restoring the blueprint editor UI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintEditorState {
    pub open_tab_ids: Vec<String>,

    #[serde(default)]
    pub active_tab_index: usize,

    #[serde(default)]
    pub graph_view_states: HashMap<String, GraphViewState>,
}

/// View state for a single graph (camera position, zoom, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphViewState {
    pub pan_offset_x: f32,
    pub pan_offset_y: f32,
    pub zoom: f32,
}

/// Class variable definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassVariable {
    pub id: String,
    pub name: String,
    pub data_type: DataType,
    pub default_value: Option<String>,
    #[serde(default)]
    pub description: String,
}

impl BlueprintAsset {
    /// Create a new empty blueprint asset
    pub fn new(name: &str) -> Self {
        Self {
            format_version: 1,
            main_graph: GraphDescription::new(name),
            local_macros: Vec::new(),
            variables: Vec::new(),
            editor_state: Some(BlueprintEditorState {
                open_tab_ids: vec!["main".to_string()],
                active_tab_index: 0,
                graph_view_states: HashMap::new(),
            }),
            blueprint_metadata: BlueprintMetadata::default(),
        }
    }
}

/// A field in a custom event definition (serializable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomEventFieldDescription {
    pub name: String,
    pub type_name: String,
}

/// Shared definition for a custom event (serializable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomEventDefDescription {
    pub name: String,
    pub uid: String,
    pub fields: Vec<CustomEventFieldDescription>,
    #[serde(default)]
    pub return_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphDescription {
    pub nodes: HashMap<String, NodeInstance>,
    pub connections: Vec<Connection>,
    pub metadata: GraphMetadata,
    #[serde(default)]
    pub comments: Vec<BlueprintComment>,
    #[serde(default)]
    pub custom_event_defs: HashMap<String, CustomEventDefDescription>,
}

/// Comment box in a blueprint graph
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlueprintComment {
    pub id: String,
    pub text: String,
    #[serde(with = "tuple_or_object_f32_pair")]
    pub position: (f32, f32),
    #[serde(with = "tuple_or_object_f32_pair")]
    pub size: (f32, f32),
    #[serde(with = "array_or_hsla_color")]
    pub color: [f32; 4],
    pub contained_node_ids: Vec<String>,
}

// Helper module to deserialise both tuple and object formats for (f32, f32)
mod tuple_or_object_f32_pair {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum F32Pair {
        Tuple(f32, f32),
        Object { x: f32, y: f32 },
        ObjectAlt { width: f32, height: f32 },
    }

    pub fn serialize<S>(value: &(f32, f32), serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<(f32, f32), D::Error>
    where
        D: Deserializer<'de>,
    {
        match F32Pair::deserialize(deserializer)? {
            F32Pair::Tuple(x, y) => Ok((x, y)),
            F32Pair::Object { x, y } => Ok((x, y)),
            F32Pair::ObjectAlt { width, height } => Ok((width, height)),
        }
    }
}

// Helper module to deserialise both array and HSLA object formats for colours
mod array_or_hsla_color {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ColorFormat {
        Array([f32; 4]),
        Hsla { h: f32, s: f32, l: f32, a: f32 },
    }

    pub fn serialize<S>(value: &[f32; 4], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[f32; 4], D::Error>
    where
        D: Deserializer<'de>,
    {
        match ColorFormat::deserialize(deserializer)? {
            ColorFormat::Array(arr) => Ok(arr),
            ColorFormat::Hsla { h, s, l, a } => {
                let (r, g, b) = hsl_to_rgb(h, s, l);
                Ok([r, g, b, a])
            }
        }
    }

    fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
        if s == 0.0 {
            return (l, l, l);
        }

        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;

        let hue_to_rgb = |p: f32, q: f32, mut t: f32| -> f32 {
            if t < 0.0 {
                t += 1.0;
            }
            if t > 1.0 {
                t -= 1.0;
            }
            if t < 1.0 / 6.0 {
                return p + (q - p) * 6.0 * t;
            }
            if t < 1.0 / 2.0 {
                return q;
            }
            if t < 2.0 / 3.0 {
                return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
            }
            p
        };

        (
            hue_to_rgb(p, q, h + 1.0 / 3.0),
            hue_to_rgb(p, q, h),
            hue_to_rgb(p, q, h - 1.0 / 3.0),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinInstance {
    pub id: String,
    #[serde(flatten)]
    pub pin: Pin,
}

#[derive(Debug, Clone)]
pub struct NodeInstance {
    pub id: String,
    pub node_type: String,
    pub position: Position,
    pub properties: HashMap<String, JsonValue>,
    pub inputs: Vec<PinInstance>,
    pub outputs: Vec<PinInstance>,
}

// Custom (de)serialisation for NodeInstance to support both array and map for inputs/outputs
impl<'de> Deserialize<'de> for NodeInstance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct NodeInstanceHelper {
            id: String,
            node_type: String,
            position: Position,
            properties: HashMap<String, JsonValue>,
            #[serde(default)]
            inputs: JsonValue,
            #[serde(default)]
            outputs: JsonValue,
        }

        let helper = NodeInstanceHelper::deserialize(deserializer)?;

        fn parse_pins(val: &serde_json::Value) -> Vec<PinInstance> {
            if let Some(arr) = val.as_array() {
                arr.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect()
            } else if let Some(obj) = val.as_object() {
                obj.iter()
                    .filter_map(|(id, v)| {
                        let pin: Pin = serde_json::from_value(v.clone()).ok()?;
                        Some(PinInstance {
                            id: id.clone(),
                            pin,
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }

        Ok(NodeInstance {
            id: helper.id,
            node_type: helper.node_type,
            position: helper.position,
            properties: helper.properties,
            inputs: parse_pins(&helper.inputs),
            outputs: parse_pins(&helper.outputs),
        })
    }
}

impl Serialize for NodeInstance {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("NodeInstance", 6)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("node_type", &self.node_type)?;
        s.serialize_field("position", &self.position)?;
        s.serialize_field("properties", &self.properties)?;
        s.serialize_field("inputs", &self.inputs)?;
        s.serialize_field("outputs", &self.outputs)?;
        s.end()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub source_node: String,
    pub source_pin: String,
    pub target_node: String,
    pub target_pin: String,
    pub connection_type: ConnectionType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pin {
    pub name: String,
    pub pin_type: PinType,
    pub data_type: DataType,
    pub connected_to: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphMetadata {
    pub name: String,
    pub description: String,
    pub version: String,
    pub created_at: String,
    pub modified_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PinType {
    Input,
    Output,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DataType {
    Execution,
    Data(TypeInfo),
}

impl DataType {
    /// Create a DataType from a type string
    pub fn from_type_str(type_str: &str) -> Self {
        let type_str = type_str.trim();

        if type_str.eq_ignore_ascii_case("execution") || type_str == "()" {
            return DataType::Execution;
        }

        DataType::Data(TypeInfo::parse(&canonicalize_legacy_type_str(type_str)))
    }

    pub fn type_info(&self) -> Option<&TypeInfo> {
        match self {
            DataType::Data(type_info) => Some(type_info),
            _ => None,
        }
    }

    pub fn rust_type_string(&self) -> String {
        self.to_string()
    }

    pub fn is_compatible_with(&self, other: &DataType) -> bool {
        match (self, other) {
            (DataType::Execution, DataType::Execution) => true,
            (DataType::Data(a), DataType::Data(b)) => a.is_compatible_with(b),
            _ => false,
        }
    }

    pub fn generate_pin_style(&self) -> PinStyle {
        match self {
            DataType::Execution => PinStyle {
                color: PinColor {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
                icon: PinIcon::Triangle,
                is_rainbow: false,
            },
            DataType::Data(type_info) => type_info.generate_pin_style(),
        }
    }
}

impl PartialEq<&str> for DataType {
    fn eq(&self, other: &&str) -> bool {
        self.to_string() == *other
    }
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataType::Execution => write!(f, "execution"),
            DataType::Data(type_info) => write!(f, "{}", type_info),
        }
    }
}

fn canonicalize_legacy_type_str(type_str: &str) -> String {
    let trimmed = type_str.trim();
    match trimmed {
        "any" | "Any" => "?".to_string(),
        "string" | "String" => "String".to_string(),
        "number" | "Number" => "f64".to_string(),
        "boolean" | "Boolean" => "bool".to_string(),
        "vector2" | "Vector2" => "(f32, f32)".to_string(),
        "vector3" | "Vector3" => "(f32, f32, f32)".to_string(),
        "color" | "Color" => "(f32, f32, f32, f32)".to_string(),
        "object" | "Object" => "dyn Any".to_string(),
        _ if trimmed.starts_with("array<") && trimmed.ends_with('>') => {
            let inner = &trimmed[6..trimmed.len() - 1];
            format!("Vec<{}>", canonicalize_legacy_type_str(inner))
        }
        _ => trimmed.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConnectionType {
    Execution,
    Data,
}

impl GraphDescription {
    pub fn new(name: &str) -> Self {
        Self {
            nodes: HashMap::new(),
            connections: Vec::new(),
            metadata: GraphMetadata {
                name: name.to_string(),
                description: String::new(),
                version: "1.0.0".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                modified_at: chrono::Utc::now().to_rfc3339(),
            },
            comments: Vec::new(),
            custom_event_defs: HashMap::new(),
        }
    }

    pub fn add_node(&mut self, node: NodeInstance) {
        self.nodes.insert(node.id.clone(), node);
        self.metadata.modified_at = chrono::Utc::now().to_rfc3339();
    }

    pub fn add_connection(&mut self, connection: Connection) {
        if let Some(source_node) = self.nodes.get_mut(&connection.source_node) {
            if let Some(output_pin) = source_node
                .outputs
                .iter_mut()
                .find(|p| p.id == connection.source_pin)
            {
                output_pin.pin.connected_to.push(connection.id.clone());
            }
        }

        if let Some(target_node) = self.nodes.get_mut(&connection.target_node) {
            if let Some(input_pin) = target_node
                .inputs
                .iter_mut()
                .find(|p| p.id == connection.target_pin)
            {
                input_pin.pin.connected_to.push(connection.id.clone());
            }
        }

        self.connections.push(connection);
        self.metadata.modified_at = chrono::Utc::now().to_rfc3339();
    }

    pub fn remove_node(&mut self, node_id: &str) {
        self.connections
            .retain(|conn| conn.source_node != node_id && conn.target_node != node_id);
        self.nodes.remove(node_id);
        self.metadata.modified_at = chrono::Utc::now().to_rfc3339();
    }

    pub fn remove_connection(&mut self, connection_id: &str) {
        if let Some(index) = self
            .connections
            .iter()
            .position(|conn| conn.id == connection_id)
        {
            let connection = &self.connections[index];

            if let Some(source_node) = self.nodes.get_mut(&connection.source_node) {
                if let Some(output_pin) = source_node
                    .outputs
                    .iter_mut()
                    .find(|p| p.id == connection.source_pin)
                {
                    output_pin.pin.connected_to.retain(|id| id != connection_id);
                }
            }

            if let Some(target_node) = self.nodes.get_mut(&connection.target_node) {
                if let Some(input_pin) = target_node
                    .inputs
                    .iter_mut()
                    .find(|p| p.id == connection.target_pin)
                {
                    input_pin.pin.connected_to.retain(|id| id != connection_id);
                }
            }

            self.connections.remove(index);
            self.metadata.modified_at = chrono::Utc::now().to_rfc3339();
        }
    }

    pub fn get_execution_order(&self) -> Result<Vec<String>, String> {
        let mut visited = HashMap::new();
        let mut temp_visited = HashMap::new();
        let mut result = Vec::new();

        for node_id in self.nodes.keys() {
            if !visited.contains_key(node_id) {
                self.visit_node(node_id, &mut visited, &mut temp_visited, &mut result)?;
            }
        }

        Ok(result)
    }

    fn visit_node(
        &self,
        node_id: &str,
        visited: &mut HashMap<String, bool>,
        temp_visited: &mut HashMap<String, bool>,
        result: &mut Vec<String>,
    ) -> Result<(), String> {
        if temp_visited.contains_key(node_id) {
            return Err(format!(
                "Circular dependency detected involving node {}",
                node_id
            ));
        }

        if visited.contains_key(node_id) {
            return Ok(());
        }

        temp_visited.insert(node_id.to_string(), true);

        for connection in &self.connections {
            if connection.source_node == node_id
                && matches!(connection.connection_type, ConnectionType::Execution)
            {
                self.visit_node(&connection.target_node, visited, temp_visited, result)?;
            }
        }

        temp_visited.remove(node_id);
        visited.insert(node_id.to_string(), true);
        result.push(node_id.to_string());

        Ok(())
    }
}

impl NodeInstance {
    pub fn new(id: &str, node_type: &str, position: Position) -> Self {
        Self {
            id: id.to_string(),
            node_type: node_type.to_string(),
            position,
            properties: HashMap::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    pub fn add_input_pin(&mut self, name: &str, data_type: DataType) {
        let pin = Pin {
            name: name.to_string(),
            pin_type: PinType::Input,
            data_type,
            connected_to: Vec::new(),
        };
        self.inputs.push(PinInstance {
            id: name.to_string(),
            pin,
        });
    }

    pub fn add_output_pin(&mut self, name: &str, data_type: DataType) {
        let pin = Pin {
            name: name.to_string(),
            pin_type: PinType::Output,
            data_type,
            connected_to: Vec::new(),
        };
        self.outputs.push(PinInstance {
            id: name.to_string(),
            pin,
        });
    }

    pub fn set_property(&mut self, name: &str, value: JsonValue) {
        self.properties.insert(name.to_string(), value);
    }
}

impl Connection {
    pub fn new(
        id: &str,
        source_node: &str,
        source_pin: &str,
        target_node: &str,
        target_pin: &str,
        connection_type: ConnectionType,
    ) -> Self {
        Self {
            id: id.to_string(),
            source_node: source_node.to_string(),
            source_pin: source_pin.to_string(),
            target_node: target_node.to_string(),
            target_pin: target_pin.to_string(),
            connection_type,
        }
    }
}

// ===== Sub-Graph System =====

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGraphDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub graph: GraphDescription,
    pub interface: SubGraphInterface,
    pub metadata: SubGraphMetadata,
    #[serde(default)]
    pub macro_config: MacroConfiguration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGraphInterface {
    pub inputs: Vec<SubGraphPin>,
    pub outputs: Vec<SubGraphPin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGraphPin {
    pub id: String,
    pub name: String,
    pub data_type: DataType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    #[serde(default)]
    pub is_instance_editable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGraphMetadata {
    pub created_at: String,
    pub modified_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroConfiguration {
    #[serde(default)]
    pub is_pure: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_node_title: Option<String>,
    #[serde(default)]
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub instance_editable_pins: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<(f32, f32, f32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub parent_class_filter: Vec<String>,
    #[serde(default)]
    pub hide_in_palette: bool,
}

impl Default for MacroConfiguration {
    fn default() -> Self {
        Self {
            is_pure: false,
            compact_node_title: None,
            category: "Macros".to_string(),
            tooltip: None,
            keywords: Vec::new(),
            instance_editable_pins: Vec::new(),
            color: None,
            icon: None,
            parent_class_filter: Vec::new(),
            hide_in_palette: false,
        }
    }
}

impl SubGraphDefinition {
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            graph: GraphDescription::new(name),
            interface: SubGraphInterface {
                inputs: Vec::new(),
                outputs: Vec::new(),
            },
            metadata: SubGraphMetadata {
                created_at: chrono::Utc::now().to_rfc3339(),
                modified_at: chrono::Utc::now().to_rfc3339(),
                author: None,
                tags: Vec::new(),
            },
            macro_config: MacroConfiguration::default(),
        }
    }

    pub fn add_input_pin(&mut self, id: &str, name: &str, data_type: DataType) {
        self.interface.inputs.push(SubGraphPin {
            id: id.to_string(),
            name: name.to_string(),
            data_type,
            description: None,
            default_value: None,
            is_instance_editable: false,
            category: None,
        });
        self.metadata.modified_at = chrono::Utc::now().to_rfc3339();
    }

    pub fn add_output_pin(&mut self, id: &str, name: &str, data_type: DataType) {
        self.interface.outputs.push(SubGraphPin {
            id: id.to_string(),
            name: name.to_string(),
            data_type,
            description: None,
            default_value: None,
            is_instance_editable: false,
            category: None,
        });
        self.metadata.modified_at = chrono::Utc::now().to_rfc3339();
    }

    pub fn sync_interface_nodes(&mut self) {
        let entry_node_id = "macro_entry";

        let has_entry = self.graph.nodes.contains_key(entry_node_id);
        let has_old_entry = self.graph.nodes.contains_key("subgraph_input");

        if has_entry || has_old_entry {
            let key = if has_entry {
                entry_node_id
            } else {
                "subgraph_input"
            };

            if let Some(entry_node) = self.graph.nodes.get_mut(key) {
                if entry_node.node_type == "subgraph_input" {
                    entry_node.node_type = "macro_entry".to_string();
                    entry_node.id = entry_node_id.to_string();
                }

                entry_node.outputs.clear();
                for pin in &self.interface.inputs {
                    entry_node.outputs.push(PinInstance {
                        id: pin.id.clone(),
                        pin: Pin {
                            name: pin.name.clone(),
                            pin_type: PinType::Output,
                            data_type: pin.data_type.clone(),
                            connected_to: Vec::new(),
                        },
                    });
                }
            }
        } else {
            let mut entry_node = NodeInstance::new(
                entry_node_id,
                "macro_entry",
                Position { x: 100.0, y: 200.0 },
            );
            for pin in &self.interface.inputs {
                entry_node.add_output_pin(&pin.id, pin.data_type.clone());
            }
            self.graph.add_node(entry_node);
        }

        if self.graph.nodes.contains_key("macro_entry")
            && self.graph.nodes.contains_key("subgraph_input")
        {
            self.graph.nodes.remove("subgraph_input");
        }

        let exit_node_id = "macro_exit";

        let has_exit = self.graph.nodes.contains_key(exit_node_id);
        let has_old_exit = self.graph.nodes.contains_key("subgraph_output");

        if has_exit || has_old_exit {
            let key = if has_exit {
                exit_node_id
            } else {
                "subgraph_output"
            };

            if let Some(exit_node) = self.graph.nodes.get_mut(key) {
                if exit_node.node_type == "subgraph_output" {
                    exit_node.node_type = "macro_exit".to_string();
                    exit_node.id = exit_node_id.to_string();
                }

                exit_node.inputs.clear();
                for pin in &self.interface.outputs {
                    exit_node.inputs.push(PinInstance {
                        id: pin.id.clone(),
                        pin: Pin {
                            name: pin.name.clone(),
                            pin_type: PinType::Input,
                            data_type: pin.data_type.clone(),
                            connected_to: Vec::new(),
                        },
                    });
                }
            }
        } else {
            let mut exit_node =
                NodeInstance::new(exit_node_id, "macro_exit", Position { x: 800.0, y: 200.0 });
            for pin in &self.interface.outputs {
                exit_node.add_input_pin(&pin.id, pin.data_type.clone());
            }
            self.graph.add_node(exit_node);
        }

        if self.graph.nodes.contains_key("macro_exit")
            && self.graph.nodes.contains_key("subgraph_output")
        {
            self.graph.nodes.remove("subgraph_output");
        }
    }

    pub fn create_instance(&self, instance_id: &str, position: Position) -> NodeInstance {
        let mut node = NodeInstance::new(instance_id, &format!("macro:{}", self.id), position);

        node.set_property("macro_id", serde_json::json!(self.id));

        if let Some(ref icon) = self.macro_config.icon {
            node.set_property("icon", serde_json::json!(icon));
        }
        if let Some((r, g, b)) = self.macro_config.color {
            node.set_property("color_r", serde_json::json!(r));
            node.set_property("color_g", serde_json::json!(g));
            node.set_property("color_b", serde_json::json!(b));
        }
        if let Some(ref title) = self.macro_config.compact_node_title {
            node.set_property("compact_title", serde_json::json!(title));
        }

        for pin in &self.interface.inputs {
            node.inputs.push(PinInstance {
                id: pin.id.clone(),
                pin: Pin {
                    name: pin.name.clone(),
                    pin_type: PinType::Input,
                    data_type: pin.data_type.clone(),
                    connected_to: Vec::new(),
                },
            });
        }

        for pin in &self.interface.outputs {
            node.outputs.push(PinInstance {
                id: pin.id.clone(),
                pin: Pin {
                    name: pin.name.clone(),
                    pin_type: PinType::Output,
                    data_type: pin.data_type.clone(),
                    connected_to: Vec::new(),
                },
            });
        }

        node
    }
}

// ===== Sub-Graph Library System =====

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGraphLibrary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub category: String,
    pub subgraphs: Vec<SubGraphDefinition>,
    pub metadata: LibraryMetadata,
    #[serde(default)]
    pub library_config: LibraryConfiguration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryConfiguration {
    #[serde(default)]
    pub is_engine_library: bool,
    #[serde(default)]
    pub is_user_library: bool,
    #[serde(default)]
    pub target_blueprint_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_color: Option<(f32, f32, f32)>,
    #[serde(default)]
    pub hot_reload_enabled: bool,
}

impl Default for LibraryConfiguration {
    fn default() -> Self {
        Self {
            is_engine_library: false,
            is_user_library: true,
            target_blueprint_types: Vec::new(),
            library_icon: None,
            library_color: None,
            hot_reload_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryMetadata {
    pub created_at: String,
    pub modified_at: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl SubGraphLibrary {
    pub fn new(id: &str, name: &str, category: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            author: None,
            category: category.to_string(),
            subgraphs: Vec::new(),
            metadata: LibraryMetadata {
                created_at: chrono::Utc::now().to_rfc3339(),
                modified_at: chrono::Utc::now().to_rfc3339(),
                tags: Vec::new(),
                icon: None,
            },
            library_config: LibraryConfiguration::default(),
        }
    }

    pub fn add_subgraph(&mut self, subgraph: SubGraphDefinition) {
        self.subgraphs.push(subgraph);
        self.metadata.modified_at = chrono::Utc::now().to_rfc3339();
    }

    pub fn get_subgraph(&self, id: &str) -> Option<&SubGraphDefinition> {
        self.subgraphs.iter().find(|sg| sg.id == id)
    }

    pub fn save_to_file(&self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load_from_file(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        let json = std::fs::read_to_string(path)?;
        let library = serde_json::from_str(&json)?;
        Ok(library)
    }
}

#[derive(Debug, Clone)]
pub struct LibraryManager {
    libraries: HashMap<String, SubGraphLibrary>,
    subgraph_cache: HashMap<String, SubGraphDefinition>,
    search_paths: Vec<std::path::PathBuf>,
}

impl LibraryManager {
    pub fn new() -> Self {
        Self {
            libraries: HashMap::new(),
            subgraph_cache: HashMap::new(),
            search_paths: Vec::new(),
        }
    }

    pub fn add_search_path(&mut self, path: impl Into<std::path::PathBuf>) {
        self.search_paths.push(path.into());
    }

    pub fn load_all_libraries(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        for search_path in &self.search_paths.clone() {
            if search_path.exists() && search_path.is_dir() {
                self.load_libraries_from_dir(search_path)?;
            }
        }
        Ok(())
    }

    fn load_libraries_from_dir(
        &mut self,
        dir: &std::path::Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                match SubGraphLibrary::load_from_file(&path) {
                    Ok(library) => {
                        self.register_library(library);
                    }
                    Err(e) => {
                        tracing::error!("Failed to load library from {:?}: {}", path, e);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn register_library(&mut self, library: SubGraphLibrary) {
        for subgraph in &library.subgraphs {
            self.subgraph_cache
                .insert(subgraph.id.clone(), subgraph.clone());
        }
        self.libraries.insert(library.id.clone(), library);
    }

    pub fn get_subgraph(&self, id: &str) -> Option<&SubGraphDefinition> {
        self.subgraph_cache.get(id)
    }

    pub fn get_libraries(&self) -> &HashMap<String, SubGraphLibrary> {
        &self.libraries
    }

    pub fn get_all_subgraphs(&self) -> Vec<&SubGraphDefinition> {
        self.subgraph_cache.values().collect()
    }

    pub fn get_subgraphs_by_category(&self, category: &str) -> Vec<&SubGraphDefinition> {
        self.libraries
            .values()
            .filter(|lib| lib.category == category)
            .flat_map(|lib| lib.subgraphs.iter())
            .collect()
    }

    pub fn default_stdlib_path() -> std::path::PathBuf {
        std::env::current_dir()
            .unwrap_or_default()
            .join("libraries")
            .join("std")
    }

    pub fn default_user_library_path() -> std::path::PathBuf {
        std::env::current_dir()
            .unwrap_or_default()
            .join("libraries")
            .join("user")
    }
}

impl Default for LibraryManager {
    fn default() -> Self {
        let mut manager = Self::new();
        manager.add_search_path(Self::default_stdlib_path());
        manager.add_search_path(Self::default_user_library_path());
        manager
    }
}
