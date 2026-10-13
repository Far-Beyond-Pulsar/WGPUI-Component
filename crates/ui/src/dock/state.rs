use gpui::{point, px, size, App, AppContext, Axis, Bounds, Entity, Pixels, WeakEntity, Window};
use itertools::Itertools as _;
use serde::{Deserialize, Serialize};

use super::{Dock, DockArea, DockItem, DockPlacement, Panel, PanelRegistry};

/// Used to serialize and deserialize the DockArea
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DockAreaState {
    /// The version is used to mark this persisted state is compatible with the current version
    /// For example, some times we many totally changed the structure of the Panel,
    /// then we can compare the version to decide whether we can use the state or ignore.
    #[serde(default)]
    pub version: Option<usize>,
    pub center: PanelState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_dock: Option<DockState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_dock: Option<DockState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bottom_dock: Option<DockState>,
    /// The zoomed group, if the dock area was zoomed when dumped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoomed: Option<ZoomedState>,
}

/// Names the zoomed tab group of a [`DockAreaState`] without using any runtime
/// id, so it survives the tree being rebuilt (and pruned) on restore.
///
/// The group is identified primarily by its active panel (see
/// [`PanelState::same_identity`]); `placement` + `path` are the fallback used
/// when no panel matches (e.g. the panel could not be restored).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZoomedState {
    /// Snapshot of the group's active leaf panel (no children).
    pub panel: PanelState,
    /// Which dock the group lives in.
    pub placement: DockPlacement,
    /// Child indices from the dock's root panel down to the group
    /// (stack children / tile indices); empty when the root is the group.
    #[serde(default)]
    pub path: Vec<usize>,
}

/// Used to serialize and deserialize the Dock
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DockState {
    panel: PanelState,
    placement: DockPlacement,
    size: Pixels,
    open: bool,
}

impl DockState {
    /// The dock's panel tree.
    pub fn panel(&self) -> &PanelState {
        &self.panel
    }

    pub fn placement(&self) -> DockPlacement {
        self.placement
    }

    /// Width (left / right) or height (bottom) of the dock.
    pub fn size(&self) -> Pixels {
        self.size
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn new(dock: Entity<Dock>, cx: &App) -> Self {
        let dock = dock.read(cx);

        Self {
            placement: dock.placement,
            size: dock.size,
            open: dock.open,
            panel: dock.panel.view().dump(cx),
        }
    }

    /// Convert the DockState to Dock
    pub fn to_dock(
        &self,
        dock_area: WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Dock> {
        let item = self.panel.to_item(dock_area.clone(), window, cx);
        cx.new(|cx| {
            Dock::from_state(
                dock_area.clone(),
                self.placement,
                self.size,
                item,
                self.open,
                window,
                cx,
            )
        })
    }
}

/// Used to serialize and deserialize the DockerItem
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PanelState {
    pub panel_name: String,
    pub children: Vec<PanelState>,
    pub info: PanelInfo,
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileMeta {
    pub bounds: Bounds<Pixels>,
    pub z_index: usize,
}

impl Default for TileMeta {
    fn default() -> Self {
        Self {
            bounds: Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(200.), px(200.)),
            },
            z_index: 0,
        }
    }
}

impl From<Bounds<Pixels>> for TileMeta {
    fn from(bounds: Bounds<Pixels>) -> Self {
        Self { bounds, z_index: 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PanelInfo {
    #[serde(rename = "stack")]
    Stack {
        sizes: Vec<Pixels>,
        axis: usize, // 0 for horizontal, 1 for vertical
    },
    #[serde(rename = "tabs")]
    Tabs { active_index: usize },
    #[serde(rename = "panel")]
    Panel(serde_json::Value),
    #[serde(rename = "tiles")]
    Tiles { metas: Vec<TileMeta> },
}

impl PanelInfo {
    pub fn stack(sizes: Vec<Pixels>, axis: Axis) -> Self {
        Self::Stack {
            sizes,
            axis: if axis == Axis::Horizontal { 0 } else { 1 },
        }
    }

    pub fn tabs(active_index: usize) -> Self {
        Self::Tabs { active_index }
    }

    pub fn panel(info: serde_json::Value) -> Self {
        Self::Panel(info)
    }

    pub fn tiles(metas: Vec<TileMeta>) -> Self {
        Self::Tiles { metas }
    }

    pub fn axis(&self) -> Option<Axis> {
        match self {
            Self::Stack { axis, .. } => Some(if *axis == 0 {
                Axis::Horizontal
            } else {
                Axis::Vertical
            }),
            _ => None,
        }
    }

    pub fn sizes(&self) -> Option<&Vec<Pixels>> {
        match self {
            Self::Stack { sizes, .. } => Some(sizes),
            _ => None,
        }
    }

    pub fn active_index(&self) -> Option<usize> {
        match self {
            Self::Tabs { active_index } => Some(*active_index),
            _ => None,
        }
    }
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            panel_name: "".to_string(),
            children: Vec::new(),
            info: PanelInfo::Panel(serde_json::Value::Null),
        }
    }
}

impl PanelState {
    pub fn new<P: Panel>(panel: &P) -> Self {
        Self {
            panel_name: panel.panel_name().to_string(),
            ..Default::default()
        }
    }

    pub fn add_child(&mut self, panel: PanelState) {
        self.children.push(panel);
    }

    /// The JSON object of a leaf, created (replacing `null` or any non-object
    /// value) when the leaf has none yet. Non-leaf infos are turned into leaves.
    fn leaf_object_mut(&mut self) -> &mut serde_json::Map<String, serde_json::Value> {
        if !matches!(&self.info, PanelInfo::Panel(v) if v.is_object()) {
            self.info = PanelInfo::Panel(serde_json::Value::Object(Default::default()));
        }
        match &mut self.info {
            PanelInfo::Panel(serde_json::Value::Object(map)) => map,
            _ => unreachable!("info was just made a leaf object"),
        }
    }

    /// Mark this leaf as editing `file`, so it can be reopened on restore.
    ///
    /// Merges with any other recorded leaf data (restore key, ephemeral marker).
    pub fn with_file(mut self, file: impl Into<String>) -> Self {
        self.leaf_object_mut()
            .insert("file".to_string(), serde_json::Value::String(file.into()));
        self
    }

    /// The file a leaf panel was editing, if it recorded one (see [`Self::with_file`]).
    pub fn file(&self) -> Option<&str> {
        match &self.info {
            PanelInfo::Panel(value) => value.get("file").and_then(|f| f.as_str()),
            _ => None,
        }
    }

    /// Record a restore key: `kind` names the restorer that can rebuild this
    /// leaf and `data` is whatever it needs. Merges with a recorded file.
    pub fn with_restore(mut self, kind: impl Into<String>, data: serde_json::Value) -> Self {
        self.leaf_object_mut().insert(
            "restore".to_string(),
            serde_json::json!({ "kind": kind.into(), "data": data }),
        );
        self
    }

    /// The restore key `(kind, data)` of a leaf, if it recorded one.
    pub fn restore_key(&self) -> Option<(&str, &serde_json::Value)> {
        let restore = match &self.info {
            PanelInfo::Panel(value) => value.get("restore")?,
            _ => return None,
        };
        Some((restore.get("kind")?.as_str()?, restore.get("data")?))
    }

    /// Mutable access to the restore key's data, for rewriting paths on save / load.
    pub fn restore_data_mut(&mut self) -> Option<&mut serde_json::Value> {
        match &mut self.info {
            PanelInfo::Panel(value) => value.get_mut("restore")?.get_mut("data"),
            _ => None,
        }
    }

    /// Declare this leaf ephemeral: it is deliberately not restored.
    pub fn ephemeral(mut self) -> Self {
        self.leaf_object_mut()
            .insert("ephemeral".to_string(), serde_json::Value::Bool(true));
        self
    }

    /// Whether the leaf was marked with [`Self::ephemeral`].
    pub fn is_ephemeral(&self) -> bool {
        match &self.info {
            PanelInfo::Panel(value) => value
                .get("ephemeral")
                .and_then(|e| e.as_bool())
                .unwrap_or(false),
            _ => false,
        }
    }

    /// Whether `self` and `other` name the same panel: same `panel_name`, same
    /// recorded file and same restore key.
    pub fn same_identity(&self, other: &PanelState) -> bool {
        self.panel_name == other.panel_name
            && self.file() == other.file()
            && self.restore_key() == other.restore_key()
    }

    /// Mutable access to the recorded file, for rewriting paths on save / load.
    pub fn file_mut(&mut self) -> Option<&mut serde_json::Value> {
        match &mut self.info {
            PanelInfo::Panel(value) => value.get_mut("file"),
            _ => None,
        }
    }

    pub fn to_item(
        &self,
        dock_area: WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut App,
    ) -> DockItem {
        let info = self.info.clone();

        let items: Vec<DockItem> = self
            .children
            .iter()
            .map(|child| child.to_item(dock_area.clone(), window, cx))
            .collect();

        match info {
            PanelInfo::Stack { sizes, axis } => {
                let axis = if axis == 0 {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                let sizes = sizes.iter().map(|s| Some(*s)).collect_vec();
                DockItem::split_with_sizes(axis, items, sizes, &dock_area, window, cx)
            }
            PanelInfo::Tabs { active_index } => {
                if items.len() == 1 {
                    return items[0].clone();
                }

                let items = items
                    .iter()
                    .flat_map(|item| match item {
                        DockItem::Tabs { items, .. } => items.clone(),
                        _ => {
                            // ignore invalid panels in tabs
                            vec![]
                        }
                    })
                    .collect_vec();

                DockItem::tabs(items, Some(active_index), &dock_area, window, cx)
            }
            PanelInfo::Panel(_) => {
                let view = PanelRegistry::build_panel(
                    &self.panel_name,
                    dock_area.clone(),
                    self,
                    &info,
                    window,
                    cx,
                );
                DockItem::tabs(vec![view.into()], None, &dock_area, window, cx)
            }
            PanelInfo::Tiles { metas } => DockItem::tiles(items, metas, &dock_area, window, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::*;

    fn leaf(name: &str) -> PanelState {
        PanelState {
            panel_name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn file_and_restore_merge_in_either_order() {
        let a = leaf("Editor")
            .with_restore("git.diff", serde_json::json!({ "a": 1 }))
            .with_file("x.rs");
        let b = leaf("Editor")
            .with_file("x.rs")
            .with_restore("git.diff", serde_json::json!({ "a": 1 }));
        assert_eq!(a, b);
        assert_eq!(a.file(), Some("x.rs"));
        let (kind, data) = a.restore_key().unwrap();
        assert_eq!(kind, "git.diff");
        assert_eq!(data, &serde_json::json!({ "a": 1 }));
        assert!(!a.is_ephemeral());
        assert!(a.ephemeral().is_ephemeral());
    }

    #[test]
    fn restore_data_is_mutable_and_identity_compares_keys() {
        let mut s = leaf("P").with_restore("k", serde_json::json!({ "path": "a" }));
        *s.restore_data_mut().unwrap() = serde_json::json!({ "path": "b" });
        assert_eq!(s.restore_key().unwrap().1, &serde_json::json!({ "path": "b" }));
        assert!(!s.same_identity(&leaf("P")));
        assert!(leaf("P").same_identity(&leaf("P")));
        assert!(leaf("P").restore_key().is_none());
        assert!(leaf("P").restore_data_mut_is_none());
    }

    #[test]
    fn zoomed_is_optional_and_skipped_when_none() {
        let state = DockAreaState::default();
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("zoomed"));
        let back: DockAreaState = serde_json::from_str(&json).unwrap();
        assert!(back.zoomed.is_none());

        let zoomed = DockAreaState {
            zoomed: Some(ZoomedState {
                panel: leaf("P").with_file("f"),
                placement: DockPlacement::Left,
                path: vec![0, 2],
            }),
            ..Default::default()
        };
        let back: DockAreaState =
            serde_json::from_str(&serde_json::to_string(&zoomed).unwrap()).unwrap();
        assert_eq!(back, zoomed);
    }

    impl PanelState {
        fn restore_data_mut_is_none(mut self) -> bool {
            self.restore_data_mut().is_none()
        }
    }

    #[test]
    fn test_deserialize_item_state() {
        let json = include_str!("../../tests/fixtures/layout.json");
        let state: DockAreaState = serde_json::from_str(json).unwrap();
        assert_eq!(state.version, None);
        assert_eq!(state.center.panel_name, "StackPanel");
        assert_eq!(state.center.children.len(), 2);
        assert_eq!(state.center.children[0].panel_name, "TabPanel");
        assert_eq!(state.center.children[1].children.len(), 1);
        assert_eq!(
            state.center.children[1].children[0].panel_name,
            "StoryContainer"
        );
        assert_eq!(state.center.children[1].panel_name, "TabPanel");

        let left_dock = state.left_dock.unwrap();
        assert_eq!(left_dock.open, true);
        assert_eq!(left_dock.size, px(350.0));
        assert_eq!(left_dock.placement, DockPlacement::Left);
        assert_eq!(left_dock.panel.panel_name, "TabPanel");
        assert_eq!(left_dock.panel.children.len(), 1);
        assert_eq!(left_dock.panel.children[0].panel_name, "StoryContainer");

        let bottom_dock = state.bottom_dock.unwrap();
        assert_eq!(bottom_dock.open, true);
        assert_eq!(bottom_dock.size, px(200.0));
        assert_eq!(bottom_dock.panel.panel_name, "TabPanel");
        assert_eq!(bottom_dock.panel.children.len(), 2);
        assert_eq!(bottom_dock.panel.children[0].panel_name, "StoryContainer");

        let right_dock = state.right_dock.unwrap();
        assert_eq!(right_dock.open, true);
        assert_eq!(right_dock.size, px(320.0));
        assert_eq!(right_dock.panel.panel_name, "TabPanel");
        assert_eq!(right_dock.panel.children.len(), 1);
        assert_eq!(right_dock.panel.children[0].panel_name, "StoryContainer");
    }
}
