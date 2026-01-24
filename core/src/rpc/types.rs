//! RPC request/response types for JSON-RPC 2.0 protocol

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// JSON-RPC 2.0 request
#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    pub id: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response
#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    pub id: Option<serde_json::Value>,
}

impl RpcResponse {
    pub fn success(id: Option<serde_json::Value>, result: impl Serialize) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: Some(serde_json::to_value(result).unwrap_or(serde_json::Value::Null)),
            error: None,
            id,
        }
    }

    pub fn error(id: Option<serde_json::Value>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(RpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
            id,
        }
    }

    pub fn parse_error() -> Self {
        Self::error(None, -32700, "Parse error")
    }

    pub fn invalid_request(id: Option<serde_json::Value>) -> Self {
        Self::error(id, -32600, "Invalid Request")
    }

    pub fn method_not_found(id: Option<serde_json::Value>) -> Self {
        Self::error(id, -32601, "Method not found")
    }

    pub fn invalid_params(id: Option<serde_json::Value>, msg: &str) -> Self {
        Self::error(id, -32602, &format!("Invalid params: {}", msg))
    }

    pub fn internal_error(id: Option<serde_json::Value>, msg: &str) -> Self {
        Self::error(id, -32603, &format!("Internal error: {}", msg))
    }
}

/// JSON-RPC 2.0 error
#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// Screenshot types

#[derive(Debug, Deserialize, Default)]
pub struct ScreenshotGetParams {
    #[serde(default)]
    pub format: ScreenshotFormat,
}

#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotFormat {
    #[default]
    Png,
    RawRgba,
}

#[derive(Debug, Serialize)]
pub struct ScreenshotGetResult {
    pub width: u16,
    pub height: u16,
    pub data: String,
    pub format: String,
}

#[derive(Debug, Deserialize)]
pub struct ScreenshotSaveParams {
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct ScreenshotSaveResult {
    pub success: bool,
    pub path: PathBuf,
}

// Status types

/// Screen resolution and color information
#[derive(Debug, Serialize)]
pub struct ScreenInfo {
    pub width: u32,
    pub height: u32,
    pub color: bool,
}

#[derive(Debug, Serialize)]
pub struct StatusGetResult {
    pub running: bool,
    pub model: String,
    pub cpu_type: String,
    pub ram_mb: u32,
    pub screen: ScreenInfo,
    pub has_adb: bool,
    pub has_scsi: bool,
    pub hd_floppy: bool,
    pub speed: String,
    pub effective_speed: f64,
    pub cycles: u64,
    pub scsi: Vec<ScsiTargetInfo>,
    pub floppy: Vec<FloppyInfo>,
    pub serial: Vec<SerialPortInfo>,
    pub shared_dir: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct SerialPortInfo {
    pub channel: String,
    pub enabled: bool,
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ScsiTargetInfo {
    pub id: usize,
    pub target_type: Option<String>,
    pub image: Option<PathBuf>,
    pub capacity: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct FloppyInfo {
    pub drive: usize,
    pub present: bool,
    pub ejected: bool,
    pub motor: bool,
    pub writing: bool,
    pub track: usize,
    pub image_title: String,
    pub dirty: bool,
}

// Config types

#[derive(Debug, Deserialize)]
pub struct ConfigSetSharedDirParams {
    pub path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct ConfigSetSharedDirResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct ConfigSerialGetParams {
    pub channel: SerialChannel,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum SerialChannel {
    A,
    B,
}

#[derive(Debug, Serialize)]
pub struct ConfigSerialGetResult {
    pub enabled: bool,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConfigSerialEnableParams {
    pub channel: SerialChannel,
    pub mode: SerialMode,
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SerialMode {
    Pty,
    Tcp,
    Localtalk,
}

#[derive(Debug, Serialize)]
pub struct ConfigSerialEnableResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SerialBridgeStatusInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum SerialBridgeStatusInfo {
    Pty {
        path: String,
    },
    Tcp {
        port: u16,
        connected: Option<String>,
    },
    LocalTalk {
        status: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct ConfigSerialDisableParams {
    pub channel: SerialChannel,
}

#[derive(Debug, Serialize)]
pub struct ConfigSerialDisableResult {
    pub success: bool,
}

// Speed types

#[derive(Debug, Serialize)]
pub struct SpeedGetResult {
    pub mode: String,
    pub effective_speed: f64,
}

#[derive(Debug, Deserialize)]
pub struct SpeedSetParams {
    pub mode: SpeedMode,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum SpeedMode {
    Accurate,
    Uncapped,
    Video,
}

#[derive(Debug, Serialize)]
pub struct SpeedSetResult {
    pub success: bool,
    pub previous: String,
}

// Mouse types

#[derive(Debug, Serialize)]
pub struct MouseGetPositionResult {
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Deserialize)]
pub struct MouseSetPositionParams {
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Serialize)]
pub struct MouseSetPositionResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct MouseMoveParams {
    pub dx: i16,
    pub dy: i16,
}

#[derive(Debug, Serialize)]
pub struct MouseMoveResult {
    pub success: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct MouseClickParams {
    pub x: Option<u16>,
    pub y: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct MouseClickResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct MouseButtonParams {
    pub state: ButtonState,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ButtonState {
    Down,
    Up,
}

#[derive(Debug, Serialize)]
pub struct MouseButtonResult {
    pub success: bool,
}

// Keyboard types

#[derive(Debug, Deserialize)]
pub struct KeyboardTypeParams {
    pub text: String,
    #[serde(default = "default_delay_ms")]
    pub delay_ms: u64,
}

fn default_delay_ms() -> u64 {
    50
}

#[derive(Debug, Serialize)]
pub struct KeyboardTypeResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct KeyboardComboParams {
    pub keys: Vec<String>,
    #[serde(default = "default_delay_ms")]
    pub delay_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct KeyboardComboResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct KeyboardKeyParams {
    pub key: KeySpec,
    pub state: ButtonState,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum KeySpec {
    Name(String),
    Scancode(u8),
}

#[derive(Debug, Serialize)]
pub struct KeyboardKeyResult {
    pub success: bool,
}

#[derive(Debug, Serialize)]
pub struct KeyboardReleaseAllResult {
    pub success: bool,
}

// Emulator control types

#[derive(Debug, Serialize)]
pub struct EmulatorControlResult {
    pub success: bool,
}

// Common success result
#[derive(Debug, Serialize)]
pub struct SuccessResult {
    pub success: bool,
}

impl SuccessResult {
    pub fn ok() -> Self {
        Self { success: true }
    }
}

// Window/fullscreen types

#[derive(Debug, Serialize)]
pub struct WindowGetFullscreenResult {
    pub fullscreen: bool,
}

#[derive(Debug, Deserialize)]
pub struct WindowSetFullscreenParams {
    pub fullscreen: bool,
}

#[derive(Debug, Serialize)]
pub struct WindowSetFullscreenResult {
    pub success: bool,
    pub fullscreen: bool,
}

// Floppy types

#[derive(Debug, Deserialize)]
pub struct FloppyInsertParams {
    pub drive: usize,
    pub path: PathBuf,
    #[serde(default)]
    pub write_protect: bool,
}

#[derive(Debug, Serialize)]
pub struct FloppyInsertResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct FloppyEjectParams {
    pub drive: usize,
}

#[derive(Debug, Serialize)]
pub struct FloppyEjectResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct FloppyStatusParams {
    pub drive: usize,
}

// CDROM types

#[derive(Debug, Deserialize)]
pub struct CdromInsertParams {
    pub id: usize,
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct CdromInsertResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct CdromEjectParams {
    pub id: usize,
}

#[derive(Debug, Serialize)]
pub struct CdromEjectResult {
    pub success: bool,
}

// SCSI types

#[derive(Debug, Deserialize)]
pub struct ScsiAttachHddParams {
    pub id: usize,
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct ScsiAttachHddResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct ScsiAttachCdromParams {
    pub id: usize,
}

#[derive(Debug, Serialize)]
pub struct ScsiAttachCdromResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct ScsiDetachParams {
    pub id: usize,
}

#[derive(Debug, Serialize)]
pub struct ScsiDetachResult {
    pub success: bool,
}

// Debugger stepping types

#[derive(Debug, Serialize)]
pub struct DebuggerStepResult {
    pub success: bool,
}

// Breakpoint types

#[derive(Debug, Deserialize)]
pub struct BreakpointSetParams {
    pub address: u32,
    #[serde(default)]
    pub bp_type: BreakpointType,
}

#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BreakpointType {
    #[default]
    Exec,
    Read,
    Write,
}

#[derive(Debug, Serialize)]
pub struct BreakpointSetResult {
    pub success: bool,
}

#[derive(Debug, Serialize)]
pub struct BreakpointInfo {
    pub address: u32,
    pub bp_type: String,
}

#[derive(Debug, Serialize)]
pub struct BreakpointListResult {
    pub breakpoints: Vec<BreakpointInfo>,
}

#[derive(Debug, Deserialize)]
pub struct BreakpointRemoveParams {
    pub address: u32,
    #[serde(default)]
    pub bp_type: BreakpointType,
}

#[derive(Debug, Serialize)]
pub struct BreakpointRemoveResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct BreakpointToggleParams {
    pub address: u32,
    #[serde(default)]
    pub bp_type: BreakpointType,
}

#[derive(Debug, Serialize)]
pub struct BreakpointToggleResult {
    pub success: bool,
    pub enabled: bool,
}

// Memory access types

#[derive(Debug, Deserialize)]
pub struct MemoryReadParams {
    pub address: u32,
    #[serde(default = "default_memory_length")]
    pub length: usize,
}

fn default_memory_length() -> usize {
    256
}

#[derive(Debug, Serialize)]
pub struct MemoryReadResult {
    pub address: u32,
    pub data: String, // base64-encoded
    pub length: usize,
}

#[derive(Debug, Deserialize)]
pub struct MemoryWriteParams {
    pub address: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Serialize)]
pub struct MemoryWriteResult {
    pub success: bool,
    pub bytes_written: usize,
}

// Register access types

#[derive(Debug, Deserialize, Default)]
pub struct RegistersGetParams {
    pub register: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegistersGetResult {
    pub d0: u32,
    pub d1: u32,
    pub d2: u32,
    pub d3: u32,
    pub d4: u32,
    pub d5: u32,
    pub d6: u32,
    pub d7: u32,
    pub a0: u32,
    pub a1: u32,
    pub a2: u32,
    pub a3: u32,
    pub a4: u32,
    pub a5: u32,
    pub a6: u32,
    pub a7: u32,
    pub pc: u32,
    pub sr: u16,
    pub usp: u32,
    pub ssp: u32,
}

#[derive(Debug, Serialize)]
pub struct RegisterGetSingleResult {
    pub register: String,
    pub value: u32,
}

#[derive(Debug, Deserialize)]
pub struct RegistersSetParams {
    pub register: String,
    pub value: u32,
}

#[derive(Debug, Serialize)]
pub struct RegistersSetResult {
    pub success: bool,
}

// Disassembly types

#[derive(Debug, Deserialize)]
pub struct DisassemblyGetParams {
    pub address: Option<u32>,
    #[serde(default = "default_disassembly_count")]
    pub count: usize,
}

fn default_disassembly_count() -> usize {
    20
}

#[derive(Debug, Serialize)]
pub struct DisassemblyEntry {
    pub address: u32,
    pub bytes: String,
    pub mnemonic: String,
    pub operands: String,
}

#[derive(Debug, Serialize)]
pub struct DisassemblyGetResult {
    pub entries: Vec<DisassemblyEntry>,
}

// Save state types

#[derive(Debug, Deserialize)]
pub struct SavestateSaveParams {
    pub path: PathBuf,
    #[serde(default)]
    pub include_screenshot: bool,
}

#[derive(Debug, Serialize)]
pub struct SavestateSaveResult {
    pub success: bool,
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct SavestateLoadParams {
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct SavestateLoadResult {
    pub success: bool,
}

// Audio control types

#[derive(Debug, Deserialize)]
pub struct AudioSetMuteParams {
    pub muted: bool,
}

#[derive(Debug, Serialize)]
pub struct AudioSetMuteResult {
    pub success: bool,
    pub muted: bool,
}

#[derive(Debug, Serialize)]
pub struct AudioGetMuteResult {
    pub muted: bool,
}

// Recording types

#[derive(Debug, Deserialize)]
pub struct RecordingStartParams {
    pub path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct RecordingStartResult {
    pub success: bool,
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct RecordingStopResult {
    pub success: bool,
}

#[derive(Debug, Serialize)]
pub struct RecordingStatusResult {
    pub recording: bool,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub struct RecordingPlayParams {
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct RecordingPlayResult {
    pub success: bool,
}

// History/tracing types

#[derive(Debug, Deserialize)]
pub struct HistoryEnableParams {
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct HistoryEnableResult {
    pub success: bool,
    pub enabled: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct HistoryGetParams {
    pub count: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct InstructionHistoryEntry {
    pub address: u32,
    pub instruction: String,
    pub cycles: u64,
}

#[derive(Debug, Serialize)]
pub struct InstructionHistoryGetResult {
    pub entries: Vec<InstructionHistoryEntry>,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct SystrapHistoryEntry {
    pub address: u32,
    pub trap_word: u16,
    pub trap_name: String,
    pub cycles: u64,
}

#[derive(Debug, Serialize)]
pub struct SystrapHistoryGetResult {
    pub entries: Vec<SystrapHistoryEntry>,
    pub enabled: bool,
}

// Peripheral debug types

#[derive(Debug, Deserialize)]
pub struct PeripheralDebugEnableParams {
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct PeripheralDebugEnableResult {
    pub success: bool,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct PeripheralStateResult {
    pub enabled: bool,
    pub peripherals: Vec<PeripheralInfo>,
}

#[derive(Debug, Serialize)]
pub struct PeripheralInfo {
    pub name: String,
    pub properties: Vec<PropertyInfo>,
}

#[derive(Debug, Serialize)]
pub struct PropertyInfo {
    pub key: String,
    pub value: String,
}

// Emulator info types

#[derive(Debug, Serialize)]
pub struct EmulatorCyclesResult {
    pub cycles: u64,
}

#[derive(Debug, Serialize)]
pub struct EmulatorProgrammerKeyResult {
    pub success: bool,
}
