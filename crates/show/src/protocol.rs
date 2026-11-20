use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::model::{MediaItem, MediaKind, Mapping, Output, Scene, Screen, Transition};

/// Everything the engine needs to render; media paths are absolute (controller resolves).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct EngineConfig {
    pub outputs: Vec<Output>,
    pub screens: Vec<Screen>,
    pub mappings: Vec<Mapping>,
    pub media: Vec<MediaItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, TS)]
pub enum Bus {
    Pvw,
    Pgm,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, TS)]
pub enum TransportAction {
    Play,
    Pause,
    Stop,
    Restart,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ToEngine {
    SetConfig { config: EngineConfig },
    SetPreview { scene: Scene },
    SetProgram { scene: Scene },
    Take { transition: Transition },
    Transport { bus: Bus, layer: Option<Uuid>, action: TransportAction },
    SetMasterVolume { volume: f32 },
    ListMonitors,
    Probe { media_id: Uuid, path: String },
    Identify { output_id: Uuid },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub refresh_mhz: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct OutputState {
    pub output_id: Uuid,
    pub bound: bool,
    pub monitor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProbeInfo {
    pub kind: MediaKind,
    pub width: u32,
    pub height: u32,
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
    pub has_audio: bool,
    pub has_alpha: bool,
    pub thumb: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum FromEngine {
    Monitors { monitors: Vec<MonitorInfo> },
    OutputStatus { outputs: Vec<OutputState> },
    Probed { media_id: Uuid, info: Option<ProbeInfo>, error: Option<String> },
    TakeDone,
    Positions {
        #[ts(type = "Record<string, number>")]
        positions: BTreeMap<Uuid, u64>,
    },
    Stats {
        fps: f32,
        #[ts(type = "number")]
        dropped: u64,
    },
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TransitionKind;

    #[test]
    fn to_engine_json_shape() {
        assert_eq!(serde_json::to_string(&ToEngine::ListMonitors).unwrap(), r#"{"type":"ListMonitors"}"#);
        let t = ToEngine::Take { transition: Transition { kind: TransitionKind::Fade, duration_ms: 500 } };
        let back: ToEngine = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
    }
}
