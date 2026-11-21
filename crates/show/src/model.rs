use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::geom::Rect;

pub type Scene = BTreeMap<Uuid, Vec<Layer>>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Show {
    pub version: u32,
    pub outputs: Vec<Output>,
    pub screens: Vec<Screen>,
    pub mappings: Vec<Mapping>,
    pub media: Vec<MediaItem>,
    pub groups: Vec<Group>,
    pub master_volume: f32,
}

impl Show {
    pub fn new() -> Show {
        Show {
            version: 1,
            outputs: vec![],
            screens: vec![],
            mappings: vec![],
            media: vec![],
            groups: vec![Group { id: Uuid::new_v4(), name: "Group 1".into(), cues: vec![] }],
            master_volume: 1.0,
        }
    }
}

impl Default for Show {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    pub id: Uuid,
    pub name: String,
    pub node: String,
    pub target: OutputTarget,
    pub width: u32,
    pub height: u32,
    pub brightness: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum OutputTarget {
    Display { monitor_name: String, monitor_x: i32, monitor_y: i32 },
    Window { x: i32, y: i32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Screen {
    pub id: Uuid,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub layout_x: i32,
    pub layout_y: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Mapping {
    pub id: Uuid,
    pub screen_id: Uuid,
    pub src: Rect,
    pub output_id: Uuid,
    pub dst: Rect,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MediaItem {
    pub id: Uuid,
    pub name: String,
    pub path: String,
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
pub enum MediaKind {
    Image,
    Video,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub cues: Vec<Cue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Cue {
    pub id: Uuid,
    pub name: String,
    pub slot: u32,
    pub layers_by_screen: Scene,
    pub transition: Transition,
    /// A key that loads this cue into Preview: `A`–`Z`, `0`–`9` or `F1`–`F12`, unique in its group.
    #[serde(default)]
    pub hotkey: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Transition {
    pub kind: TransitionKind,
    pub duration_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub enum TransitionKind {
    Cut,
    Fade,
    WipeLeft,
    WipeRight,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Layer {
    pub id: Uuid,
    pub name: String,
    pub source: Source,
    pub rect: Rect,
    pub opacity: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub volume: f32,
    pub playback: Playback,
    pub visible: bool,
    /// Silent, whatever its volume (kept for unmuting).
    #[serde(default)]
    pub muted: bool,
    /// Degrees clockwise about the rect's centre.
    #[serde(default)]
    pub rotation: f32,
    /// Playback rate, 0.25–4 (a clip is silent unless it is 1).
    #[serde(default = "one")]
    pub speed: f32,
    /// Play only part of a clip: from `trim_in_ms` into the file to `trim_out_ms` (none: its end).
    #[serde(default)]
    #[ts(type = "number")]
    pub trim_in_ms: u64,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub trim_out_ms: Option<u64>,
}

fn one() -> f32 {
    1.0
}

impl Layer {
    pub fn new(source: Source, rect: Rect) -> Layer {
        Layer {
            id: Uuid::new_v4(),
            name: String::new(),
            source,
            rect,
            opacity: 1.0,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            volume: 1.0,
            playback: Playback::Loop,
            visible: true,
            muted: false,
            rotation: 0.0,
            speed: 1.0,
            trim_in_ms: 0,
            trim_out_ms: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum Source {
    Media { media_id: Uuid },
    Pattern { pattern: Pattern },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum Pattern {
    Bars,
    Grid,
    Gradient,
    Solid { rgba: [f32; 4] },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub enum Playback {
    Loop,
    Once,
    HoldLast,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Fields added after a show was saved load with their defaults.
    #[test]
    fn older_cue_json_loads() {
        let json = r#"{"id":"00000000-0000-4000-8000-000000000001","name":"C","slot":0,"layersByScreen":{},"transition":{"kind":"Cut","durationMs":0}}"#;
        let cue: Cue = serde_json::from_str(json).unwrap();
        assert_eq!(cue.hotkey, None);
        let layer = r#"{"id":"00000000-0000-4000-8000-000000000002","name":"","source":{"type":"Pattern","pattern":{"type":"Bars"}},
            "rect":{"x":0,"y":0,"w":10,"h":10},"opacity":1,"brightness":0,"contrast":1,"saturation":1,"volume":1,"playback":"Loop","visible":true}"#;
        let layer: Layer = serde_json::from_str(layer).unwrap();
        assert_eq!(layer, Layer { id: layer.id, ..Layer::new(layer.source.clone(), layer.rect.clone()) });
    }

    #[test]
    fn show_json_roundtrip() {
        let mut show = Show::new();
        let out = Output { id: Uuid::new_v4(), name: "O1".into(), node: "local".into(),
            target: OutputTarget::Display { monitor_name: "M".into(), monitor_x: 0, monitor_y: 0 },
            width: 1920, height: 1080, brightness: 1.0 };
        let scr = Screen { id: Uuid::new_v4(), name: "S".into(), width: 1920, height: 1080, layout_x: 0, layout_y: 0 };
        let full = Rect { x: 0., y: 0., w: 1920., h: 1080. };
        show.mappings.push(Mapping { id: Uuid::new_v4(), screen_id: scr.id, src: full.clone(), output_id: out.id, dst: full.clone() });
        let layer = Layer::new(Source::Pattern { pattern: Pattern::Solid { rgba: [1., 0., 0., 1.] } }, full);
        let mut cue = Cue { id: Uuid::new_v4(), name: "C".into(), slot: 0, layers_by_screen: Scene::new(),
            transition: Transition { kind: TransitionKind::Fade, duration_ms: 500 }, hotkey: Some("H".into()) };
        cue.layers_by_screen.insert(scr.id, vec![layer]);
        show.groups[0].cues.push(cue);
        show.outputs.push(out);
        show.screens.push(scr);
        let json = serde_json::to_string(&show).unwrap();
        assert!(json.contains("\"type\":\"Display\"") && json.contains("\"layersByScreen\""));
        assert_eq!(serde_json::from_str::<Show>(&json).unwrap(), show);
    }
}
