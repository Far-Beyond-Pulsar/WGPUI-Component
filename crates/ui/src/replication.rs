//! Multi-user editing and state replication system.
//!
//! Provides the infrastructure for replicating UI component state across
//! multiple connected users in real-time collaborative editing sessions.
//! All types live here directly — there is no dependency on any engine crate.
//!
//! ## Core Concepts
//!
//! - [`ReplicationMode`]: Defines how an element's state should be shared
//! - [`Replicator`]: Trait that makes components network-aware
//! - [`ReplicationRegistry`]: Tracks which users are editing which elements
//! - [`UserPresence`]: Represents a connected user's state

use gpui::{App, Global, Hsla};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── Presence ─────────────────────────────────────────────────────────────────

fn default_color() -> Hsla {
    gpui::hsla(0.5, 0.7, 0.6, 1.0)
}

/// Represents a user's presence in the collaborative session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPresence {
    pub peer_id: String,
    pub display_name: String,
    #[serde(skip, default = "default_color")]
    pub color: Hsla,
    pub current_panel: Option<String>,
    pub editing_element: Option<String>,
    pub cursor_position: Option<usize>,
    pub selection: Option<(usize, usize)>,
    pub last_activity: u64,
    pub is_idle: bool,
    pub status: Option<String>,
}

impl UserPresence {
    pub fn new(peer_id: impl Into<String>, display_name: impl Into<String>, color: Hsla) -> Self {
        Self {
            peer_id: peer_id.into(),
            display_name: display_name.into(),
            color,
            current_panel: None,
            editing_element: None,
            cursor_position: None,
            selection: None,
            last_activity: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            is_idle: false,
            status: None,
        }
    }

    /// Get initials (up to 2 chars) for display in presence pills
    pub fn initials(&self) -> String {
        self.display_name
            .split_whitespace()
            .take(2)
            .filter_map(|w| w.chars().next())
            .collect::<String>()
            .to_uppercase()
    }

    /// Shortened display name (first name only)
    pub fn short_name(&self) -> &str {
        self.display_name
            .split_whitespace()
            .next()
            .unwrap_or(&self.display_name)
    }

    pub fn update_activity(&mut self) {
        self.last_activity = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.is_idle = false;
    }

    pub fn check_idle(&mut self, idle_threshold: Duration) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.is_idle = (now - self.last_activity) >= idle_threshold.as_secs();
    }
}

// ── Mode / Config ─────────────────────────────────────────────────────────────

/// Defines how a UI element's state should be replicated across users
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ReplicationMode {
    #[default]
    NoRep,
    MultiEdit,
    LockedEdit,
    RequestEdit,
    BroadcastOnly,
    Follow,
    QueuedEdit,
    PartitionedEdit,
}

impl ReplicationMode {
    pub fn is_collaborative(&self) -> bool {
        matches!(
            self,
            ReplicationMode::MultiEdit
                | ReplicationMode::Follow
                | ReplicationMode::QueuedEdit
                | ReplicationMode::PartitionedEdit
        )
    }

    pub fn is_exclusive(&self) -> bool {
        matches!(
            self,
            ReplicationMode::LockedEdit
                | ReplicationMode::RequestEdit
                | ReplicationMode::BroadcastOnly
        )
    }

    pub fn requires_permission(&self) -> bool {
        matches!(self, ReplicationMode::RequestEdit)
    }

    pub fn shows_presence(&self) -> bool {
        !matches!(
            self,
            ReplicationMode::NoRep | ReplicationMode::BroadcastOnly
        )
    }

    pub fn is_realtime(&self) -> bool {
        matches!(
            self,
            ReplicationMode::MultiEdit | ReplicationMode::BroadcastOnly | ReplicationMode::Follow
        )
    }

    pub fn description(&self) -> &'static str {
        match self {
            ReplicationMode::NoRep => "Local only - not shared with other users",
            ReplicationMode::MultiEdit => "Collaborative - all users can edit simultaneously",
            ReplicationMode::LockedEdit => "Exclusive - only one user can edit at a time",
            ReplicationMode::RequestEdit => "Moderated - requires approval to edit",
            ReplicationMode::BroadcastOnly => "Presentation - host controls, clients watch",
            ReplicationMode::Follow => "Follow mode - sync with another user's view",
            ReplicationMode::QueuedEdit => "Sequential - changes applied in order",
            ReplicationMode::PartitionedEdit => "Partitioned - each user has their own section",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            ReplicationMode::NoRep => "user",
            ReplicationMode::MultiEdit => "users",
            ReplicationMode::LockedEdit => "lock",
            ReplicationMode::RequestEdit => "hand",
            ReplicationMode::BroadcastOnly => "radio",
            ReplicationMode::Follow => "eye",
            ReplicationMode::QueuedEdit => "list-ordered",
            ReplicationMode::PartitionedEdit => "layers",
        }
    }
}

/// Strategy for resolving conflicts when multiple users edit simultaneously
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConflictStrategy {
    #[default]
    LastWriteWins,
    FirstWriteWins,
    Manual,
    OperationalTransform,
    CRDT,
}

/// Configuration for replication behaviour
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationConfig {
    pub mode: ReplicationMode,
    pub show_presence: bool,
    pub show_cursors: bool,
    pub debounce_ms: u32,
    pub max_concurrent_editors: Option<usize>,
    pub track_history: bool,
    pub conflict_strategy: Option<ConflictStrategy>,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            mode: ReplicationMode::NoRep,
            show_presence: true,
            show_cursors: true,
            debounce_ms: 100,
            max_concurrent_editors: None,
            track_history: false,
            conflict_strategy: None,
        }
    }
}

impl ReplicationConfig {
    pub fn new(mode: ReplicationMode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }

    pub fn with_presence(mut self, show: bool) -> Self {
        self.show_presence = show;
        self
    }

    pub fn with_cursors(mut self, show: bool) -> Self {
        self.show_cursors = show;
        self
    }

    pub fn with_debounce(mut self, ms: u32) -> Self {
        self.debounce_ms = ms;
        self
    }

    pub fn with_max_editors(mut self, max: usize) -> Self {
        self.max_concurrent_editors = Some(max);
        self
    }

    pub fn with_history(mut self) -> Self {
        self.track_history = true;
        self
    }

    pub fn with_conflict_strategy(mut self, strategy: ConflictStrategy) -> Self {
        self.conflict_strategy = Some(strategy);
        self
    }
}

// ── State ──────────────────────────────────────────────────────────────────────

/// State tracking for a single replicated element
#[derive(Debug, Clone)]
pub struct ElementReplicationState {
    pub element_id: String,
    pub config: ReplicationConfig,
    pub last_state: Option<Value>,
    pub last_update: u64,
    pub active_editors: Vec<String>,
    pub locked_by: Option<String>,
    pub pending_requests: Vec<String>,
}

impl ElementReplicationState {
    pub fn new(element_id: String, config: ReplicationConfig) -> Self {
        Self {
            element_id,
            config,
            last_state: None,
            last_update: 0,
            active_editors: Vec::new(),
            locked_by: None,
            pending_requests: Vec::new(),
        }
    }

    pub fn can_edit(&self, peer_id: &str) -> bool {
        match self.config.mode {
            ReplicationMode::NoRep => true,
            ReplicationMode::MultiEdit => {
                if let Some(max) = self.config.max_concurrent_editors {
                    if self.active_editors.len() >= max
                        && !self.active_editors.contains(&peer_id.to_string())
                    {
                        return false;
                    }
                }
                true
            }
            ReplicationMode::LockedEdit => {
                self.locked_by.is_none() || self.locked_by.as_deref() == Some(peer_id)
            }
            ReplicationMode::RequestEdit => self.active_editors.contains(&peer_id.to_string()),
            ReplicationMode::BroadcastOnly => peer_id == "0",
            ReplicationMode::Follow => true,
            ReplicationMode::QueuedEdit => true,
            ReplicationMode::PartitionedEdit => true,
        }
    }

    pub fn acquire_lock(&mut self, peer_id: &str) -> bool {
        if self.config.mode != ReplicationMode::LockedEdit {
            return true;
        }
        if self.locked_by.is_none() {
            self.locked_by = Some(peer_id.to_string());
            true
        } else {
            false
        }
    }

    pub fn release_lock(&mut self, peer_id: &str) {
        if self.locked_by.as_deref() == Some(peer_id) {
            self.locked_by = None;
        }
    }

    pub fn request_permission(&mut self, peer_id: &str) {
        if self.config.mode != ReplicationMode::RequestEdit {
            return;
        }
        if !self.pending_requests.contains(&peer_id.to_string()) {
            self.pending_requests.push(peer_id.to_string());
        }
    }

    pub fn grant_permission(&mut self, peer_id: &str) {
        self.pending_requests.retain(|id| id != peer_id);
        if !self.active_editors.contains(&peer_id.to_string()) {
            self.active_editors.push(peer_id.to_string());
        }
    }

    pub fn revoke_permission(&mut self, peer_id: &str) {
        self.active_editors.retain(|id| id != peer_id);
    }
}

// ── Registry ───────────────────────────────────────────────────────────────────

struct RegistryInner {
    elements: HashMap<String, ElementReplicationState>,
    panel_presences: HashMap<String, Vec<String>>,
    user_presences: HashMap<String, UserPresence>,
    on_state_change: Option<Box<dyn Fn(&str, &Value) + Send + Sync>>,
}

impl RegistryInner {
    fn new() -> Self {
        Self {
            elements: HashMap::new(),
            panel_presences: HashMap::new(),
            user_presences: HashMap::new(),
            on_state_change: None,
        }
    }
}

/// Global registry of all replicated elements in the application
pub struct ReplicationRegistry {
    inner: Arc<RwLock<RegistryInner>>,
}

impl Global for ReplicationRegistry {}

impl Clone for ReplicationRegistry {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl ReplicationRegistry {
    pub fn init(cx: &mut App) {
        cx.set_global(Self {
            inner: Arc::new(RwLock::new(RegistryInner::new())),
        });
    }

    pub fn global(cx: &App) -> Self {
        cx.global::<Self>().clone()
    }

    pub fn register_element(&self, element_id: String, config: ReplicationConfig) {
        let state = ElementReplicationState::new(element_id.clone(), config);
        self.inner.write().elements.insert(element_id, state);
    }

    pub fn unregister_element(&self, element_id: &str) {
        self.inner.write().elements.remove(element_id);
    }

    pub fn get_element_state(&self, element_id: &str) -> Option<ElementReplicationState> {
        self.inner.read().elements.get(element_id).cloned()
    }

    pub fn update_element_state(&self, element_id: &str, state: Value, timestamp: u64) -> bool {
        let mut inner = self.inner.write();
        if let Some(elem_state) = inner.elements.get_mut(element_id) {
            elem_state.last_state = Some(state.clone());
            elem_state.last_update = timestamp;
            if let Some(callback) = inner.on_state_change.as_ref() {
                callback(element_id, &state);
            }
            true
        } else {
            false
        }
    }

    pub fn add_editor(&self, element_id: &str, peer_id: &str) -> bool {
        let mut inner = self.inner.write();
        if let Some(state) = inner.elements.get_mut(element_id) {
            if state.can_edit(peer_id) && !state.active_editors.contains(&peer_id.to_string()) {
                state.active_editors.push(peer_id.to_string());
                return true;
            }
        }
        false
    }

    pub fn remove_editor(&self, element_id: &str, peer_id: &str) {
        let mut inner = self.inner.write();
        if let Some(state) = inner.elements.get_mut(element_id) {
            state.active_editors.retain(|id| id != peer_id);
            state.release_lock(peer_id);
        }
    }

    pub fn get_editors(&self, element_id: &str) -> Vec<String> {
        self.inner
            .read()
            .elements
            .get(element_id)
            .map(|state| state.active_editors.clone())
            .unwrap_or_default()
    }

    pub fn add_panel_presence(&self, panel_id: &str, peer_id: &str) {
        self.inner
            .write()
            .panel_presences
            .entry(panel_id.to_string())
            .or_insert_with(Vec::new)
            .push(peer_id.to_string());
    }

    pub fn remove_panel_presence(&self, panel_id: &str, peer_id: &str) {
        let mut inner = self.inner.write();
        if let Some(users) = inner.panel_presences.get_mut(panel_id) {
            users.retain(|id| id != peer_id);
            if users.is_empty() {
                inner.panel_presences.remove(panel_id);
            }
        }
    }

    pub fn get_panel_users(&self, panel_id: &str) -> Vec<String> {
        self.inner
            .read()
            .panel_presences
            .get(panel_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn update_user_presence(&self, presence: UserPresence) {
        self.inner
            .write()
            .user_presences
            .insert(presence.peer_id.clone(), presence);
    }

    pub fn get_user_presence(&self, peer_id: &str) -> Option<UserPresence> {
        self.inner.read().user_presences.get(peer_id).cloned()
    }

    pub fn get_all_presences(&self) -> Vec<UserPresence> {
        self.inner.read().user_presences.values().cloned().collect()
    }

    pub fn remove_user_presence(&self, peer_id: &str) {
        let mut inner = self.inner.write();
        inner.user_presences.remove(peer_id);
        for users in inner.panel_presences.values_mut() {
            users.retain(|id| id != peer_id);
        }
        inner.panel_presences.retain(|_, users| !users.is_empty());
        for state in inner.elements.values_mut() {
            state.active_editors.retain(|id| id != peer_id);
            if state.locked_by.as_deref() == Some(peer_id) {
                state.locked_by = None;
            }
            state.pending_requests.retain(|id| id != peer_id);
        }
    }

    pub fn on_state_change<F>(&self, callback: F)
    where
        F: Fn(&str, &Value) + Send + Sync + 'static,
    {
        self.inner.write().on_state_change = Some(Box::new(callback));
    }

    pub fn clear(&self) {
        let mut inner = self.inner.write();
        inner.elements.clear();
        inner.panel_presences.clear();
        inner.user_presences.clear();
    }
}

// ── Session Context ────────────────────────────────────────────────────────────

struct SessionInner {
    our_peer_id: Option<String>,
    host_peer_id: Option<String>,
    is_active: bool,
    message_sender: Option<Box<dyn Fn(ReplicationMessage) + Send + Sync>>,
    permission_handler: Option<Box<dyn Fn(&str, &str) -> bool + Send + Sync>>,
    active_edits: HashMap<String, u64>,
}

impl SessionInner {
    fn new() -> Self {
        Self {
            our_peer_id: None,
            host_peer_id: None,
            is_active: false,
            message_sender: None,
            permission_handler: None,
            active_edits: HashMap::new(),
        }
    }
}

/// Global context for the current multiuser session.
pub struct SessionContext {
    inner: Arc<RwLock<SessionInner>>,
}

impl Global for SessionContext {}

impl Clone for SessionContext {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Default for SessionContext {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionContext {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(SessionInner::new())),
        }
    }

    pub fn init(cx: &mut App) {
        cx.set_global(Self::new());
    }

    pub fn global(cx: &App) -> Self {
        cx.global::<Self>().clone()
    }

    pub fn set_our_peer_id(&self, peer_id: String) {
        self.inner.write().our_peer_id = Some(peer_id);
    }

    pub fn our_peer_id(&self) -> Option<String> {
        self.inner.read().our_peer_id.clone()
    }

    pub fn set_host_peer_id(&self, peer_id: String) {
        self.inner.write().host_peer_id = Some(peer_id);
    }

    pub fn host_peer_id(&self) -> Option<String> {
        self.inner.read().host_peer_id.clone()
    }

    pub fn are_we_host(&self) -> bool {
        let inner = self.inner.read();
        match (&inner.our_peer_id, &inner.host_peer_id) {
            (Some(our), Some(host)) => our == host,
            _ => false,
        }
    }

    pub fn start_session(&self, our_peer_id: String, host_peer_id: String) {
        tracing::info!("Started multiuser session (host: {})", host_peer_id);
        let mut inner = self.inner.write();
        inner.our_peer_id = Some(our_peer_id);
        inner.host_peer_id = Some(host_peer_id);
        inner.is_active = true;
    }

    pub fn end_session(&self) {
        tracing::info!("Ended multiuser session");
        let mut inner = self.inner.write();
        inner.is_active = false;
        inner.our_peer_id = None;
        inner.host_peer_id = None;
        inner.active_edits.clear();
    }

    pub fn is_active(&self) -> bool {
        self.inner.read().is_active
    }

    pub fn set_message_sender<F>(&self, sender: F)
    where
        F: Fn(ReplicationMessage) + Send + Sync + 'static,
    {
        self.inner.write().message_sender = Some(Box::new(sender));
    }

    pub fn send_message(&self, message: ReplicationMessage) {
        if let Some(sender) = self.inner.read().message_sender.as_ref() {
            sender(message);
        } else {
            tracing::warn!("Tried to send replication message but no sender configured");
        }
    }

    pub fn set_permission_handler<F>(&self, handler: F)
    where
        F: Fn(&str, &str) -> bool + Send + Sync + 'static,
    {
        self.inner.write().permission_handler = Some(Box::new(handler));
    }

    pub fn request_permission(&self, element_id: &str) -> bool {
        if !self.are_we_host() {
            return false;
        }
        let inner = self.inner.read();
        if let Some(handler) = inner.permission_handler.as_ref() {
            let our_id = inner.our_peer_id.clone().unwrap_or_default();
            handler(element_id, &our_id)
        } else {
            true
        }
    }

    pub fn start_editing(&self, element_id: String) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.inner
            .write()
            .active_edits
            .insert(element_id, timestamp);
    }

    pub fn stop_editing(&self, element_id: &str) {
        self.inner.write().active_edits.remove(element_id);
    }

    pub fn is_editing(&self, element_id: &str) -> bool {
        self.inner.read().active_edits.contains_key(element_id)
    }

    pub fn active_edits(&self) -> Vec<String> {
        self.inner.read().active_edits.keys().cloned().collect()
    }
}

// ── Messages ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReplicationMessage {
    StateUpdate {
        element_id: String,
        state: Value,
        timestamp: u64,
        peer_id: String,
    },
    EditorJoined {
        element_id: String,
        peer_id: String,
    },
    EditorLeft {
        element_id: String,
        peer_id: String,
    },
    PanelJoined {
        panel_id: String,
        peer_id: String,
    },
    PanelLeft {
        panel_id: String,
        peer_id: String,
    },
    PresenceUpdate {
        peer_id: String,
        presence: UserPresence,
    },
    RequestLock {
        element_id: String,
        peer_id: String,
    },
    ReleaseLock {
        element_id: String,
        peer_id: String,
    },
    LockGranted {
        element_id: String,
        peer_id: String,
    },
    LockDenied {
        element_id: String,
        peer_id: String,
        reason: String,
    },
    RequestPermission {
        element_id: String,
        peer_id: String,
    },
    PermissionGranted {
        element_id: String,
        peer_id: String,
    },
    PermissionDenied {
        element_id: String,
        peer_id: String,
        reason: String,
    },
    RequestSync {
        element_id: String,
        peer_id: String,
    },
    SyncResponse {
        element_id: String,
        state: Value,
        timestamp: u64,
    },
}

/// Helper to create replication messages
pub struct ReplicationMessageBuilder;

impl ReplicationMessageBuilder {
    pub fn state_update(
        element_id: impl Into<String>,
        state: Value,
        peer_id: impl Into<String>,
    ) -> ReplicationMessage {
        ReplicationMessage::StateUpdate {
            element_id: element_id.into(),
            state,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            peer_id: peer_id.into(),
        }
    }

    pub fn editor_joined(
        element_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> ReplicationMessage {
        ReplicationMessage::EditorJoined {
            element_id: element_id.into(),
            peer_id: peer_id.into(),
        }
    }

    pub fn editor_left(
        element_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> ReplicationMessage {
        ReplicationMessage::EditorLeft {
            element_id: element_id.into(),
            peer_id: peer_id.into(),
        }
    }

    pub fn panel_joined(
        panel_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> ReplicationMessage {
        ReplicationMessage::PanelJoined {
            panel_id: panel_id.into(),
            peer_id: peer_id.into(),
        }
    }

    pub fn panel_left(
        panel_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> ReplicationMessage {
        ReplicationMessage::PanelLeft {
            panel_id: panel_id.into(),
            peer_id: peer_id.into(),
        }
    }

    pub fn presence_update(presence: UserPresence) -> ReplicationMessage {
        let peer_id = presence.peer_id.clone();
        ReplicationMessage::PresenceUpdate { peer_id, presence }
    }

    pub fn request_lock(
        element_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> ReplicationMessage {
        ReplicationMessage::RequestLock {
            element_id: element_id.into(),
            peer_id: peer_id.into(),
        }
    }

    pub fn release_lock(
        element_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> ReplicationMessage {
        ReplicationMessage::ReleaseLock {
            element_id: element_id.into(),
            peer_id: peer_id.into(),
        }
    }

    pub fn request_permission(
        element_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> ReplicationMessage {
        ReplicationMessage::RequestPermission {
            element_id: element_id.into(),
            peer_id: peer_id.into(),
        }
    }

    pub fn request_sync(
        element_id: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> ReplicationMessage {
        ReplicationMessage::RequestSync {
            element_id: element_id.into(),
            peer_id: peer_id.into(),
        }
    }
}

// ── Message handler ────────────────────────────────────────────────────────────

pub struct ReplicationMessageHandler {
    registry: ReplicationRegistry,
}

impl ReplicationMessageHandler {
    pub fn new(cx: &App) -> Self {
        Self {
            registry: ReplicationRegistry::global(cx),
        }
    }

    pub fn handle_message(&mut self, message: ReplicationMessage) -> Option<ReplicationMessage> {
        match message {
            ReplicationMessage::StateUpdate {
                element_id,
                state,
                timestamp,
                peer_id,
            } => {
                self.handle_state_update(&element_id, state, timestamp, &peer_id);
                None
            }
            ReplicationMessage::EditorJoined {
                element_id,
                peer_id,
            } => {
                self.handle_editor_joined(&element_id, &peer_id);
                None
            }
            ReplicationMessage::EditorLeft {
                element_id,
                peer_id,
            } => {
                self.handle_editor_left(&element_id, &peer_id);
                None
            }
            ReplicationMessage::PanelJoined { panel_id, peer_id } => {
                self.handle_panel_joined(&panel_id, &peer_id);
                None
            }
            ReplicationMessage::PanelLeft { panel_id, peer_id } => {
                self.handle_panel_left(&panel_id, &peer_id);
                None
            }
            ReplicationMessage::PresenceUpdate { peer_id, presence } => {
                self.handle_presence_update(&peer_id, presence);
                None
            }
            ReplicationMessage::RequestLock {
                element_id,
                peer_id,
            } => self.handle_lock_request(&element_id, &peer_id),
            ReplicationMessage::ReleaseLock {
                element_id,
                peer_id,
            } => {
                self.handle_lock_release(&element_id, &peer_id);
                None
            }
            ReplicationMessage::RequestPermission {
                element_id,
                peer_id,
            } => {
                self.handle_permission_request(&element_id, &peer_id);
                None
            }
            ReplicationMessage::RequestSync {
                element_id,
                peer_id,
            } => self.handle_sync_request(&element_id, &peer_id),
            ReplicationMessage::LockGranted { .. }
            | ReplicationMessage::LockDenied { .. }
            | ReplicationMessage::PermissionGranted { .. }
            | ReplicationMessage::PermissionDenied { .. }
            | ReplicationMessage::SyncResponse { .. } => None,
        }
    }

    fn handle_state_update(
        &mut self,
        element_id: &str,
        state: Value,
        timestamp: u64,
        peer_id: &str,
    ) {
        if let Some(elem_state) = self.registry.get_element_state(element_id) {
            if timestamp <= elem_state.last_update {
                return;
            }
            if !elem_state.can_edit(peer_id) {
                return;
            }
        }
        self.registry
            .update_element_state(element_id, state, timestamp);
    }

    fn handle_editor_joined(&mut self, element_id: &str, peer_id: &str) {
        self.registry.add_editor(element_id, peer_id);
    }

    fn handle_editor_left(&mut self, element_id: &str, peer_id: &str) {
        self.registry.remove_editor(element_id, peer_id);
    }

    fn handle_panel_joined(&mut self, panel_id: &str, peer_id: &str) {
        self.registry.add_panel_presence(panel_id, peer_id);
    }

    fn handle_panel_left(&mut self, panel_id: &str, peer_id: &str) {
        self.registry.remove_panel_presence(panel_id, peer_id);
    }

    fn handle_presence_update(&mut self, _peer_id: &str, presence: UserPresence) {
        self.registry.update_user_presence(presence);
    }

    fn handle_lock_request(
        &mut self,
        element_id: &str,
        peer_id: &str,
    ) -> Option<ReplicationMessage> {
        if let Some(mut elem_state) = self.registry.get_element_state(element_id) {
            if elem_state.acquire_lock(peer_id) {
                return Some(ReplicationMessage::LockGranted {
                    element_id: element_id.to_string(),
                    peer_id: peer_id.to_string(),
                });
            } else {
                let holder = elem_state.locked_by.as_deref().unwrap_or("unknown");
                return Some(ReplicationMessage::LockDenied {
                    element_id: element_id.to_string(),
                    peer_id: peer_id.to_string(),
                    reason: format!("Locked by {}", holder),
                });
            }
        }
        None
    }

    fn handle_lock_release(&mut self, element_id: &str, peer_id: &str) {
        if let Some(mut elem_state) = self.registry.get_element_state(element_id) {
            elem_state.release_lock(peer_id);
        }
    }

    fn handle_permission_request(&mut self, element_id: &str, peer_id: &str) {
        if let Some(mut elem_state) = self.registry.get_element_state(element_id) {
            elem_state.request_permission(peer_id);
        }
    }

    fn handle_sync_request(
        &mut self,
        element_id: &str,
        _peer_id: &str,
    ) -> Option<ReplicationMessage> {
        if let Some(elem_state) = self.registry.get_element_state(element_id) {
            if let Some(state) = elem_state.last_state {
                return Some(ReplicationMessage::SyncResponse {
                    element_id: element_id.to_string(),
                    state,
                    timestamp: elem_state.last_update,
                });
            }
        }
        None
    }
}

// ── Traits ─────────────────────────────────────────────────────────────────────

use gpui::{Entity, Window};

/// Trait for components that can replicate their state across users
pub trait Replicator: Sized {
    fn replication_id(&self) -> String;
    fn replication_config(&self) -> &ReplicationConfig;
    fn replication_config_mut(&mut self) -> &mut ReplicationConfig;

    fn set_replication_mode(&mut self, mode: ReplicationMode) {
        self.replication_config_mut().mode = mode;
    }

    fn serialize_state(&self, cx: &App) -> Result<Value, String>;

    fn deserialize_state(
        &mut self,
        state: Value,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<(), String>;

    fn on_remote_user_joined(&mut self, peer_id: &str, _window: &mut Window, _cx: &mut App) {
        tracing::debug!(
            "User {} started editing element {}",
            peer_id,
            self.replication_id()
        );
    }

    fn on_remote_user_left(&mut self, peer_id: &str, _window: &mut Window, _cx: &mut App) {
        tracing::debug!(
            "User {} stopped editing element {}",
            peer_id,
            self.replication_id()
        );
    }

    fn on_remote_state_update(
        &mut self,
        peer_id: &str,
        state: Value,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<bool, String> {
        let config = self.replication_config();
        let session = SessionContext::global(cx);
        let registry = ReplicationRegistry::global(cx);
        let element_id = self.replication_id();

        match config.mode {
            ReplicationMode::NoRep => {
                return Ok(false);
            }
            ReplicationMode::BroadcastOnly => {
                if let Some(host_id) = session.host_peer_id() {
                    if peer_id != host_id {
                        return Ok(false);
                    }
                } else {
                    return Ok(false);
                }
            }
            ReplicationMode::LockedEdit => {
                if let Some(elem_state) = registry.get_element_state(&element_id) {
                    if let Some(lock_holder) = &elem_state.locked_by {
                        if lock_holder != peer_id {
                            return Ok(false);
                        }
                    }
                }
            }
            ReplicationMode::RequestEdit => {
                if let Some(elem_state) = registry.get_element_state(&element_id) {
                    if !elem_state.active_editors.contains(&peer_id.to_string()) {
                        return Ok(false);
                    }
                }
            }
            _ => {}
        }

        self.deserialize_state(state, window, cx)?;
        Ok(true)
    }

    fn request_edit_permission(&mut self, _window: &mut Window, cx: &mut App) -> bool {
        let config = self.replication_config();
        if config.mode != ReplicationMode::RequestEdit {
            return true;
        }

        let session = SessionContext::global(cx);
        let registry = ReplicationRegistry::global(cx);
        let element_id = self.replication_id();

        if session.are_we_host() {
            return session.request_permission(&element_id);
        }

        if let Some(our_peer_id) = session.our_peer_id() {
            let message = ReplicationMessageBuilder::request_permission(&element_id, &our_peer_id);
            session.send_message(message);

            if let Some(mut elem_state) = registry.get_element_state(&element_id) {
                elem_state.request_permission(&our_peer_id);
            }
        }

        false
    }

    fn can_edit(&self, cx: &App) -> bool {
        let config = self.replication_config();
        let session = SessionContext::global(cx);
        let registry = ReplicationRegistry::global(cx);
        let element_id = self.replication_id();

        if !session.is_active() {
            return true;
        }

        let our_peer_id = match session.our_peer_id() {
            Some(id) => id,
            None => return false,
        };

        match config.mode {
            ReplicationMode::NoRep => true,
            ReplicationMode::MultiEdit => {
                if let Some(elem_state) = registry.get_element_state(&element_id) {
                    if let Some(max) = config.max_concurrent_editors {
                        if elem_state.active_editors.len() >= max
                            && !elem_state.active_editors.contains(&our_peer_id)
                        {
                            return false;
                        }
                    }
                }
                true
            }
            ReplicationMode::LockedEdit => {
                if let Some(elem_state) = registry.get_element_state(&element_id) {
                    elem_state.locked_by.is_none()
                        || elem_state.locked_by.as_ref() == Some(&our_peer_id)
                } else {
                    true
                }
            }
            ReplicationMode::RequestEdit => {
                if let Some(elem_state) = registry.get_element_state(&element_id) {
                    elem_state.active_editors.contains(&our_peer_id)
                } else {
                    false
                }
            }
            ReplicationMode::BroadcastOnly => session.are_we_host(),
            ReplicationMode::Follow => true,
            ReplicationMode::QueuedEdit => true,
            ReplicationMode::PartitionedEdit => true,
        }
    }
}

/// Extension trait for `Entity<T>` where `T: Replicator`
pub trait ReplicatorExt<T: Replicator> {
    fn with_replication(self, mode: ReplicationMode, cx: &mut App) -> Self;
    fn sync_state(&self, cx: &mut App);
    fn subscribe_to_replication(&self, cx: &mut App);
}

impl<T: Replicator + 'static> ReplicatorExt<T> for Entity<T> {
    fn with_replication(self, mode: ReplicationMode, cx: &mut App) -> Self {
        let element_id = self.read(cx).replication_id();
        let config = self.read(cx).replication_config().clone();

        self.update(cx, |this, _cx| {
            this.set_replication_mode(mode);
        });

        let registry = ReplicationRegistry::global(cx);
        registry.register_element(element_id, config);

        self
    }

    fn sync_state(&self, cx: &mut App) {
        let session = SessionContext::global(cx);
        let registry = ReplicationRegistry::global(cx);

        let element_id = self.read(cx).replication_id();
        let state = match self.read(cx).serialize_state(cx) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to serialize state for {}: {}", element_id, e);
                return;
            }
        };

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        registry.update_element_state(&element_id, state.clone(), timestamp);

        if session.is_active() {
            if let Some(our_peer_id) = session.our_peer_id() {
                let message =
                    ReplicationMessageBuilder::state_update(element_id.clone(), state, our_peer_id);
                session.send_message(message);
            }
        }
    }

    fn subscribe_to_replication(&self, cx: &mut App) {
        let element_id = self.read(cx).replication_id();
        let config = self.read(cx).replication_config().clone();
        let registry = ReplicationRegistry::global(cx);
        registry.register_element(element_id, config);
    }
}

/// Trait for panels/tabs that can show user presence
pub trait PresenceAware {
    fn active_users(&self) -> Vec<String>;
    fn add_user_presence(&mut self, peer_id: String);
    fn remove_user_presence(&mut self, peer_id: &str);

    fn has_user(&self, peer_id: &str) -> bool {
        self.active_users().iter().any(|id| id == peer_id)
    }

    fn user_count(&self) -> usize {
        self.active_users().len()
    }
}

// ── Integration ────────────────────────────────────────────────────────────────

/// Helper to integrate replication with a multiuser client
pub struct MultiuserIntegration;

impl MultiuserIntegration {
    pub fn new(cx: &App) -> Self {
        if cx.try_global::<SessionContext>().is_none() {
            panic!("SessionContext not initialized. Call ui::replication::init(cx) first.");
        }
        Self
    }

    pub fn start_session<F>(
        &self,
        our_peer_id: String,
        host_peer_id: String,
        send_callback: F,
        cx: &App,
    ) where
        F: Fn(ReplicationMessage) + Send + Sync + 'static,
    {
        let session = SessionContext::global(cx);
        session.start_session(our_peer_id.clone(), host_peer_id);
        session.set_message_sender(send_callback);
        tracing::info!(
            "Multiuser replication session started (peer: {})",
            our_peer_id
        );
    }

    pub fn end_session(&self, cx: &App) {
        let session = SessionContext::global(cx);
        session.end_session();
        let registry = ReplicationRegistry::global(cx);
        registry.clear();
    }

    pub fn handle_incoming_message(
        &self,
        message: ReplicationMessage,
        cx: &App,
    ) -> Option<ReplicationMessage> {
        let mut handler = ReplicationMessageHandler::new(cx);
        handler.handle_message(message)
    }

    pub fn broadcast_message(&self, message: ReplicationMessage, cx: &App) {
        let session = SessionContext::global(cx);
        session.send_message(message);
    }

    pub fn add_user(&self, peer_id: String, display_name: String, color: Hsla, cx: &App) {
        let presence = UserPresence::new(peer_id.clone(), display_name, color);
        let registry = ReplicationRegistry::global(cx);
        registry.update_user_presence(presence);
    }

    pub fn remove_user(&self, peer_id: &str, cx: &App) {
        let registry = ReplicationRegistry::global(cx);
        registry.remove_user_presence(peer_id);
    }

    pub fn update_user_presence(&self, presence: UserPresence, cx: &App) {
        let registry = ReplicationRegistry::global(cx);
        registry.update_user_presence(presence);
    }

    pub fn set_permission_handler<F>(&self, handler: F, cx: &App)
    where
        F: Fn(&str, &str) -> bool + Send + Sync + 'static,
    {
        let session = SessionContext::global(cx);
        session.set_permission_handler(handler);
    }

    pub fn session_context(&self, cx: &App) -> SessionContext {
        SessionContext::global(cx)
    }

    pub fn registry(&self, cx: &App) -> ReplicationRegistry {
        ReplicationRegistry::global(cx)
    }
}

/// Initialize the replication system globals
pub fn init(cx: &mut App) {
    ReplicationRegistry::init(cx);
    SessionContext::init(cx);
}

// ── UI-specific extension traits ─────────────────────────────────────────────

use crate::input::{InputState, RopeExt};

/// Extension trait for [`InputState`] to add replication support.
pub trait InputStateReplicationExt {
    fn enable_replication(&self, mode: ReplicationMode, cx: &mut App);
    fn sync_if_replicated(&self, cx: &mut App);
    fn replication_mode(&self, cx: &App) -> Option<ReplicationMode>;
    fn can_edit_replicated(&self, cx: &App) -> bool;
    fn apply_remote_state(
        &self,
        state: Value,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<(), String>;
}

impl InputStateReplicationExt for Entity<InputState> {
    fn enable_replication(&self, mode: ReplicationMode, cx: &mut App) {
        let element_id = format!("input_{}", self.entity_id());
        let config = ReplicationConfig::new(mode)
            .with_debounce(100)
            .with_presence(true)
            .with_cursors(true);

        let registry = ReplicationRegistry::global(cx);
        registry.register_element(element_id, config);
    }

    fn sync_if_replicated(&self, cx: &mut App) {
        let element_id = format!("input_{}", self.entity_id());
        let registry = ReplicationRegistry::global(cx);

        if let Some(_elem_state) = registry.get_element_state(&element_id) {
            let session = SessionContext::global(cx);

            let text_rope = self.read(cx).text();
            let cursor_pos = self.read(cx).cursor();

            let state = serde_json::json!({
                "text": text_rope.to_string(),
                "cursor": cursor_pos,
            });

            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            registry.update_element_state(&element_id, state.clone(), timestamp);

            if session.is_active() {
                if let Some(our_peer_id) = session.our_peer_id() {
                    let message =
                        ReplicationMessageBuilder::state_update(element_id, state, our_peer_id);
                    session.send_message(message);
                }
            }
        }
    }

    fn replication_mode(&self, cx: &App) -> Option<ReplicationMode> {
        let element_id = format!("input_{}", self.entity_id());
        let registry = ReplicationRegistry::global(cx);
        registry
            .get_element_state(&element_id)
            .map(|state| state.config.mode)
    }

    fn can_edit_replicated(&self, cx: &App) -> bool {
        let element_id = format!("input_{}", self.entity_id());
        let registry = ReplicationRegistry::global(cx);
        let session = SessionContext::global(cx);

        if !session.is_active() {
            return true;
        }

        let elem_state = match registry.get_element_state(&element_id) {
            Some(state) => state,
            None => return true,
        };

        let our_peer_id = match session.our_peer_id() {
            Some(id) => id,
            None => return false,
        };

        match elem_state.config.mode {
            ReplicationMode::NoRep => true,
            ReplicationMode::MultiEdit => {
                if let Some(max) = elem_state.config.max_concurrent_editors {
                    elem_state.active_editors.len() < max
                        || elem_state.active_editors.contains(&our_peer_id)
                } else {
                    true
                }
            }
            ReplicationMode::LockedEdit => {
                elem_state.locked_by.is_none()
                    || elem_state.locked_by.as_ref() == Some(&our_peer_id)
            }
            ReplicationMode::RequestEdit => elem_state.active_editors.contains(&our_peer_id),
            ReplicationMode::BroadcastOnly => session.are_we_host(),
            _ => true,
        }
    }

    fn apply_remote_state(
        &self,
        state: Value,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<(), String> {
        let text = state
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or("Missing text field")?;

        let cursor = state
            .get("cursor")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        self.update(cx, |input_state, cx| {
            input_state.set_value(text.to_string(), window, cx);

            if let Some(cursor_pos) = cursor {
                let position = input_state.text().offset_to_position(cursor_pos);
                input_state.set_cursor_position(position, window, cx);
            }
        });

        Ok(())
    }
}

// ── UI-specific presence components ─────────────────────────────────────────
// These render components depend on GPUI UI primitives.

use crate::{h_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};
use gpui::{
    div, prelude::FluentBuilder, px, AnyElement, IntoElement, ParentElement, RenderOnce, Styled,
};

/// Size variant for presence pill display
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PresencePillSize {
    Small,
    Medium,
    Large,
}

/// A small coloured badge representing a connected user
#[derive(IntoElement)]
pub struct PresencePill {
    presence: UserPresence,
    show_name: bool,
    show_status: bool,
    size: PresencePillSize,
}

impl PresencePill {
    pub fn new(presence: UserPresence) -> Self {
        Self {
            presence,
            show_name: true,
            show_status: false,
            size: PresencePillSize::Medium,
        }
    }

    pub fn small(mut self) -> Self {
        self.size = PresencePillSize::Small;
        self.show_name = false;
        self
    }
    pub fn medium(mut self) -> Self {
        self.size = PresencePillSize::Medium;
        self.show_name = false;
        self
    }
    pub fn large(mut self) -> Self {
        self.size = PresencePillSize::Large;
        self.show_name = true;
        self
    }
    pub fn with_name(mut self, show: bool) -> Self {
        self.show_name = show;
        self
    }
    pub fn with_status(mut self, show: bool) -> Self {
        self.show_status = show;
        self
    }
}

impl RenderOnce for PresencePill {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.presence.color;
        let is_idle = self.presence.is_idle;
        match self.size {
            PresencePillSize::Small => div()
                .size_2()
                .rounded_full()
                .bg(color)
                .when(is_idle, |this| this.opacity(0.4)),
            PresencePillSize::Medium => div()
                .flex()
                .items_center()
                .justify_center()
                .size_6()
                .rounded_full()
                .bg(color)
                .text_color(gpui::white())
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .when(is_idle, |this| this.opacity(0.5))
                .child(self.presence.initials()),
            PresencePillSize::Large => h_flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_full()
                .bg(color.opacity(0.15))
                .border_1()
                .border_color(color)
                .when(is_idle, |this| this.opacity(0.6))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size_5()
                        .rounded_full()
                        .bg(color)
                        .text_color(gpui::white())
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(self.presence.initials()),
                )
                .when(self.show_name, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().foreground)
                            .child(self.presence.short_name().to_string()),
                    )
                })
                .when(self.show_status && self.presence.status.is_some(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.presence.status.as_ref().unwrap().clone()),
                    )
                }),
        }
    }
}

/// Overlapping stack of presence pills showing multiple connected users
#[derive(IntoElement)]
pub struct PresenceStack {
    presences: Vec<UserPresence>,
    max_visible: usize,
    show_count: bool,
    size: PresencePillSize,
}

impl PresenceStack {
    pub fn new(presences: Vec<UserPresence>) -> Self {
        Self {
            presences,
            max_visible: 3,
            show_count: true,
            size: PresencePillSize::Medium,
        }
    }

    pub fn max_visible(mut self, max: usize) -> Self {
        self.max_visible = max;
        self
    }
    pub fn show_count(mut self, show: bool) -> Self {
        self.show_count = show;
        self
    }
    pub fn small(mut self) -> Self {
        self.size = PresencePillSize::Small;
        self
    }
}

impl RenderOnce for PresenceStack {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let total = self.presences.len();
        let overflow = total.saturating_sub(self.max_visible);

        h_flex()
            .items_center()
            .gap_0p5()
            .children(
                self.presences
                    .iter()
                    .take(self.max_visible)
                    .map(|presence| {
                        let pill = PresencePill::new(presence.clone());
                        match self.size {
                            PresencePillSize::Small => pill.small(),
                            PresencePillSize::Medium => pill.medium(),
                            PresencePillSize::Large => pill.large(),
                        }
                    }),
            )
            .when(self.show_count && overflow > 0, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size_6()
                        .rounded_full()
                        .bg(cx.theme().muted)
                        .border_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("+{}", overflow)),
                )
            })
    }
}

/// A 2px coloured bar at the top of a tab showing who is present
#[derive(IntoElement)]
pub struct TabPresenceIndicator {
    presences: Vec<UserPresence>,
    show_count: bool,
}

impl TabPresenceIndicator {
    pub fn new(presences: Vec<UserPresence>) -> Self {
        Self {
            presences,
            show_count: true,
        }
    }

    pub fn show_count(mut self, show: bool) -> Self {
        self.show_count = show;
        self
    }
}

impl RenderOnce for TabPresenceIndicator {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.presences.is_empty() {
            return div().into_any_element();
        }

        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(2.0))
            .bg(self.presences[0].color)
            .into_any_element()
    }
}

/// Inline indicator on an input field showing who is editing (and if locked)
#[derive(IntoElement)]
pub struct FieldPresenceIndicator {
    presence: UserPresence,
    is_locked: bool,
}

impl FieldPresenceIndicator {
    pub fn new(presence: UserPresence) -> Self {
        Self {
            presence,
            is_locked: false,
        }
    }

    pub fn locked(mut self, locked: bool) -> Self {
        self.is_locked = locked;
        self
    }
}

impl RenderOnce for FieldPresenceIndicator {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.presence.color;
        h_flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded(cx.theme().radius)
            .bg(color.opacity(0.1))
            .border_1()
            .border_color(color)
            .when(self.is_locked, |this| {
                this.child(Icon::new(IconName::Lock).size_3().text_color(color))
            })
            .child(
                div()
                    .size_4()
                    .rounded_full()
                    .bg(color)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .child(self.presence.initials()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().foreground)
                    .child(self.presence.short_name().to_string()),
            )
    }
}
